// SPDX-License-Identifier: Apache-2.0
// CH347F (WCH USB-HS bridge) の JTAG インターフェースを WebUSB で叩く層。
//
// プロトコルは openFPGALoader の src/ch347jtag.cpp (Apache-2.0,
// Copyright (C) 2023 Alexey Starikovskiy) を JavaScript に書き直したもの。
// その上に JTAG TAP ステートマシンを載せて shiftIR/shiftDR を提供する。
//
// ★WebUSB の transferIn/transferOut には libusb のような timeout 引数が無く、
//   デバイスが応答しないと Promise が永久に解決しない。全転送を withTimeout で
//   包むこと。ここを省くと「無反応でハングし、原因が分からない」障害になる。

const VID = 0x1a86;
// PID -> JTAG が載っている USB インターフェース番号
//   CH347F = 複合デバイス (UART0/UART1 が CDC-ACM、JTAG は IF4 の vendor class 0xff)
//   CH347T = モード切替式で IF2
const JTAG_INTF = { 0x55de: 4, 0x55dd: 2 };

const EP_OUT = 6;   // 0x06
const EP_IN = 6;    // 0x86 (WebUSB は方向ビットを除いた番号で指定する)
const MAX_BUFFER = 512;
const XFER_TIMEOUT_MS = 5000;

const CMD_CLK = 0xd0;
const CMD_BITS_WO = 0xd1;   // ビット単位 write only
const CMD_BITS_WR = 0xd2;   // ビット単位 write + read
const CMD_BYTES_WO = 0xd3;  // バイト単位 write only
const CMD_BYTES_WR = 0xd4;  // バイト単位 write + read

const SIG_TCK = 0b00001;
const SIG_TMS = 0b00010;
const SIG_TDI = 0b10000;

// setClk の factor に対応する TCK 周波数 (CH347F / 新しめの CH347T)
export const CLOCK_TABLE = [
  468750, 937500, 1875000, 3750000, 7500000, 15000000, 30000000, 60000000,
];

// --- JTAG TAP ステートマシン ---------------------------------------------
// [tms=0 のときの遷移先, tms=1 のときの遷移先]
const TAP_NEXT = {
  TLR:   ['RTI',   'TLR'],
  RTI:   ['RTI',   'SELDR'],
  SELDR: ['CAPDR', 'SELIR'],
  CAPDR: ['SHDR',  'E1DR'],
  SHDR:  ['SHDR',  'E1DR'],
  E1DR:  ['PADR',  'UPDR'],
  PADR:  ['PADR',  'E2DR'],
  E2DR:  ['SHDR',  'UPDR'],
  UPDR:  ['RTI',   'SELDR'],
  SELIR: ['CAPIR', 'TLR'],
  CAPIR: ['SHIR',  'E1IR'],
  SHIR:  ['SHIR',  'E1IR'],
  E1IR:  ['PAIR',  'UPIR'],
  PAIR:  ['PAIR',  'E2IR'],
  E2IR:  ['SHIR',  'UPIR'],
  UPIR:  ['RTI',   'SELDR'],
};

// 状態間の最短 TMS 列を BFS で作る。手書きの遷移表はタイプミスが入りやすく、
// 入ったときの症状 (たまに1ビットずれる) が非常に追いにくいので機械生成する。
// 同じ状態への経路は空列 = 「動かない」。SHDR→SHDR が空になることが重要で、
// これが CS を握ったまま SPI を続けるのに要る。
const TAP_PATH = (() => {
  const all = Object.keys(TAP_NEXT);
  const paths = {};
  for (const from of all) {
    const prev = { [from]: null };
    const queue = [from];
    while (queue.length) {
      const s = queue.shift();
      for (const tms of [0, 1]) {
        const n = TAP_NEXT[s][tms];
        if (n in prev) continue;
        prev[n] = [s, tms];
        queue.push(n);
      }
    }
    paths[from] = {};
    for (const to of all) {
      const bits = [];
      let cur = to;
      while (prev[cur]) { bits.unshift(prev[cur][1]); cur = prev[cur][0]; }
      paths[from][to] = bits;
    }
  }
  return paths;
})();

const REV = new Uint8Array(256);
for (let i = 0; i < 256; i++) {
  let r = 0;
  for (let b = 0; b < 8; b++) if (i & (1 << b)) r |= 0x80 >> b;
  REV[i] = r;
}
export const reverseByte = (b) => REV[b & 0xff];

const withTimeout = (promise, ms, what) => Promise.race([
  promise,
  new Promise((_, reject) =>
    setTimeout(() => reject(new Error(`USB転送がタイムアウトしました (${ms}ms): ${what}`)), ms)),
]);

export class CH347Jtag {
  constructor() {
    this.device = null;
    this.intf = null;
    this.clockHz = 0;
    this.desynced = false;   // プロトコルがずれたら true。復帰には再接続が要る
    this._state = 'TLR';
    this._tms = 0;
    this._tdi = 0;
  }

  static get filters() {
    return Object.keys(JTAG_INTF).map((pid) => ({ vendorId: VID, productId: Number(pid) }));
  }

  async connect() {
    this.device = await navigator.usb.requestDevice({ filters: CH347Jtag.filters });
    this.intf = JTAG_INTF[this.device.productId];
    if (this.intf === undefined) throw new Error(`未対応の PID: 0x${this.device.productId.toString(16)}`);
    await this.device.open();
    if (this.device.configuration === null) await this.device.selectConfiguration(1);
    // IF0-IF3 は CDC-ACM で OS のシリアルドライバが握っているが、JTAG は別
    // インターフェースなので claim できる。ここが通れば経路は成立している。
    await this.device.claimInterface(this.intf);
    this._state = 'TLR';
  }

  async disconnect() {
    if (!this.device) return;
    try { await this.device.releaseInterface(this.intf); } catch (e) { /* 切断済み */ }
    try { await this.device.close(); } catch (e) { /* 同上 */ }
    this.device = null;
  }

  get info() {
    if (!this.device) return null;
    const d = this.device;
    return {
      manufacturer: d.manufacturerName, product: d.productName, serial: d.serialNumber,
      pid: d.productId, interface: this.intf,
    };
  }

  async _out(bytes) {
    const r = await withTimeout(
      this.device.transferOut(EP_OUT, bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)),
      XFER_TIMEOUT_MS, 'transferOut');
    if (r.status !== 'ok') { this.desynced = true; throw new Error(`transferOut 失敗: ${r.status}`); }
  }

  // 1回の transferIn で応答が全部来るとは限らない。CH347 はデータをクロック
  // アウトしながら小分けに返してくるので、期待バイト数に達するまで読み継ぐ。
  // openFPGALoader も usb_xfer で while (rlen) { ... rlen -= actual_length; }
  // と累積している。実機の 509 バイト読み出しでここを踏んだ (2026-09-04)。
  // 要求長は常に 512 (bulk の最大パケット長)。短く要求すると babble になる。
  async _in(expected) {
    const out = new Uint8Array(expected);
    let got = 0;
    while (got < expected) {
      const r = await withTimeout(this.device.transferIn(EP_IN, MAX_BUFFER), XFER_TIMEOUT_MS, 'transferIn');
      if (r.status !== 'ok') { this.desynced = true; throw new Error(`transferIn 失敗: ${r.status}`); }
      const chunk = new Uint8Array(r.data.buffer, r.data.byteOffset, r.data.byteLength);
      if (chunk.length === 0) { this.desynced = true; throw new Error('応答が途切れました (0バイト)'); }
      if (got + chunk.length > expected) {
        this.desynced = true;
        throw new Error(`応答が長すぎます (${got + chunk.length} > ${expected})`);
      }
      out.set(chunk, got);
      got += chunk.length;
    }
    return out;
  }

  /** 1パケットだけ読む (前回の中断で残ったデータを捨てるのに使う)。 */
  async _inPacket() {
    const r = await withTimeout(this.device.transferIn(EP_IN, MAX_BUFFER), XFER_TIMEOUT_MS, 'transferIn');
    return new Uint8Array(r.data.buffer, r.data.byteOffset, r.data.byteLength);
  }

  /** TCK 周波数を設定する。要求値以下で最大のものが選ばれる。 */
  async setClock(hz) {
    let idx = 0;
    for (let i = 0; i < CLOCK_TABLE.length; i++) if (hz >= CLOCK_TABLE[i]) idx = i;
    await this._out([CMD_CLK, 6, 0, 0, idx, 0, 0, 0, 0]);
    // 前回が異常終了していると IN エンドポイントに読み残しがある。ここは応答が
    // 4バイトと短く形も決まっているので、正しい応答が出るまで読み捨てて同期を
    // 取り直す。WebUSB には「タイムアウト付きの空読み」が無い (待つと永久に
    // 解決しない) ので、期待している通信そのものでリカバリする。
    let rep = null;
    for (let i = 0; i < 8; i++) {
      const pkt = await this._inPacket();
      if (pkt.length === 4 && pkt[0] === CMD_CLK) { rep = pkt; break; }
    }
    if (!rep || rep[3] !== 0) {
      this.desynced = true;
      throw new Error('クロック設定に失敗しました(応答の同期が取れません)。USBを挿し直してください。');
    }
    this.desynced = false;
    this.clockHz = CLOCK_TABLE[idx];
    return this.clockHz;
  }

  /** TMS 列を送る。TDI は指定レベルに保持される。 */
  async writeTMS(bits, tdi = 1) {
    if (bits.length === 0) return;
    this._tdi = tdi ? SIG_TDI : 0;
    let val = 0;
    const body = [];
    for (const b of bits) {
      this._tms = b ? SIG_TMS : 0;
      val = this._tms | this._tdi;
      body.push(val, val | SIG_TCK);
    }
    body.push(val);  // 最後に TCK を下げる
    // 1パケットに収まらないときは分割する (ヘッダ3 + 本体)
    for (let off = 0; off < body.length; ) {
      const chunk = Math.min(body.length - off, MAX_BUFFER - 3);
      const pkt = new Uint8Array(chunk + 3);
      pkt[0] = CMD_BITS_WO;
      pkt[1] = chunk & 0xff;
      pkt[2] = chunk >> 8;
      pkt.set(body.slice(off, off + chunk), 3);
      await this._out(pkt);
      off += chunk;
    }
  }

  /** TAP を指定状態へ移す。同じ状態なら何もしない (SHIFT-DR 保持に必要)。 */
  async gotoState(target) {
    const bits = TAP_PATH[this._state][target];
    if (bits.length) await this.writeTMS(bits);
    this._state = target;
  }

  async resetTap() {
    await this.writeTMS([1, 1, 1, 1, 1]);
    this._state = 'TLR';
  }

  /**
   * TDI へ len ビット送り、必要なら TDO を受け取る。
   * end=true のとき最後の1ビットで TMS を上げ、EXIT1 で抜ける。
   */
  async writeTDI(tx, wantRx, len, end) {
    if (len === 0) return wantRx ? new Uint8Array(0) : null;
    const nbytes = Math.floor((len - (end ? 1 : 0)) / 8);
    const nbits = len - nbytes * 8;
    const rx = wantRx ? new Uint8Array(Math.ceil(len / 8)) : null;

    // --- バイト単位の部分 ---
    for (let off = 0; off < nbytes; ) {
      const chunk = Math.min(nbytes - off, MAX_BUFFER - 3);
      const pkt = new Uint8Array(chunk + 3);
      pkt[0] = wantRx ? CMD_BYTES_WR : CMD_BYTES_WO;
      pkt[1] = chunk & 0xff;
      pkt[2] = chunk >> 8;
      if (tx) pkt.set(tx.subarray(off, off + chunk), 3);
      await this._out(pkt);
      if (wantRx) {
        const rep = await this._in(chunk + 3);
        const size = rep[1] | (rep[2] << 8);
        if (rep[0] !== CMD_BYTES_WR || size !== chunk) {
          this.desynced = true;
          throw new Error(`バイト読み出しの応答が不正です (cmd=${hex2(rep[0])} size=${size} 期待=${chunk})`);
        }
        rx.set(rep.subarray(3, 3 + size), off);
      }
      off += chunk;
    }
    if (nbits === 0) return rx;

    // --- 端数ビットの部分 (end のとき最後のビットで TMS を上げる) ---
    const body = [];
    let x = 0;
    for (let i = 0; i < nbits; i++) {
      const txb = tx ? (tx[nbytes + (i >> 3)] || 0) : 0;
      this._tdi = (txb & (1 << (i & 7))) ? SIG_TDI : 0;
      x = this._tdi;
      if (end && i === nbits - 1) { this._tms = SIG_TMS; x |= SIG_TMS; }
      body.push(x, x | SIG_TCK);
    }
    body.push(x & ~SIG_TCK);
    const pkt = new Uint8Array(body.length + 3);
    pkt[0] = wantRx ? CMD_BITS_WR : CMD_BITS_WO;
    pkt[1] = body.length & 0xff;
    pkt[2] = body.length >> 8;
    pkt.set(body, 3);
    await this._out(pkt);
    if (!wantRx) return rx;

    // ビット読み出しの応答は「1ビットにつき1バイト (0x01 / 0x00)」で返る
    const rep = await this._in(nbits + 3);
    const size = rep[1] | (rep[2] << 8);
    if (rep[0] !== CMD_BITS_WR || size !== nbits) {
      this.desynced = true;
      throw new Error(`ビット読み出しの応答が不正です (cmd=${hex2(rep[0])} size=${size} 期待=${nbits})`);
    }
    for (let i = 0; i < size; i++) {
      if (rep[3 + i] === 0x01) rx[nbytes + (i >> 3)] |= 1 << (i & 7);
      else rx[nbytes + (i >> 3)] &= ~(1 << (i & 7));
    }
    return rx;
  }

  /** RUN-TEST/IDLE などで n クロック回す。 */
  async toggleClk(n) {
    const bits = this._tms | this._tdi;
    if (bits === 0 && n > 7) { await this.writeTDI(null, false, n, false); return; }
    const perPacket = Math.floor((MAX_BUFFER - 4) / 2);
    while (n > 0) {
      const cnt = Math.min(n, perPacket);
      const body = [];
      for (let i = 0; i < cnt; i++) body.push(bits, bits | SIG_TCK);
      body.push(bits);
      const pkt = new Uint8Array(body.length + 3);
      pkt[0] = CMD_BITS_WO;
      pkt[1] = body.length & 0xff;
      pkt[2] = body.length >> 8;
      pkt.set(body, 3);
      await this._out(pkt);
      n -= cnt;
    }
  }

  async shiftIR(inst, bitlen = 8, endState = 'RTI') {
    await this.gotoState('SHIR');
    const end = endState !== 'SHIR';
    await this.writeTDI(Uint8Array.of(inst), false, bitlen, end);
    if (end) { this._state = 'E1IR'; await this.gotoState(endState); }
  }

  async shiftDR(tx, wantRx, bitlen, endState = 'RTI') {
    await this.gotoState('SHDR');
    const end = endState !== 'SHDR';
    const rx = await this.writeTDI(tx, wantRx, bitlen, end);
    if (end) { this._state = 'E1DR'; await this.gotoState(endState); }
    return rx;
  }
}

export const hex2 = (v) => v.toString(16).padStart(2, '0');
export const hex8 = (v) => (v >>> 0).toString(16).padStart(8, '0');
