// docs/flash/ の JS を、ECP5 + SPI フラッシュのシミュレータ相手に通す。
// 実機を止めずに TAP 遷移・ビット順・SPI 手順・書き込みループを検証する。
import { CH347Jtag, reverseByte } from './ch347.js';
import { Ecp5Flasher, parseLatticeBit } from './ecp5.js';
import { readFileSync, existsSync } from 'fs';

const NEXT = {
  TLR:['RTI','TLR'], RTI:['RTI','SELDR'], SELDR:['CAPDR','SELIR'], CAPDR:['SHDR','E1DR'],
  SHDR:['SHDR','E1DR'], E1DR:['PADR','UPDR'], PADR:['PADR','E2DR'], E2DR:['SHDR','UPDR'],
  UPDR:['RTI','SELDR'], SELIR:['CAPIR','TLR'], CAPIR:['SHIR','E1IR'], SHIR:['SHIR','E1IR'],
  E1IR:['PAIR','UPIR'], PAIR:['PAIR','E2IR'], E2IR:['SHIR','UPIR'], UPIR:['RTI','SELDR'],
};
const IDCODE = 0x41111043;
const FLASH_SIZE = 4 * 1024 * 1024;

class Ecp5Sim {
  constructor() {
    this.state = 'TLR';
    this.ir = 0xe0; this.irSh = [0,0,0,0,0,0,0,0];
    this.dr = []; this.drVal = [];
    this.statusReg = 0x100;           // DONE
    this.spiMode = false;
    this.flash = new Uint8Array(FLASH_SIZE).fill(0xff);
    this.fstatus = 0x1c;              // BP0-2 が立った出荷状態を模す
    this.wel = 0;
    this.cs = false;
    this.stats = { erased: 0, programmed: 0, refresh: 0, sramCleared: 0 };
  }
  bits(v, n) { const a = []; for (let i = 0; i < n; i++) a.push((v >>> i) & 1); return a; }

  captureDr() {
    switch (this.ir) {
      case 0xe0: this.dr = this.bits(IDCODE, 32); break;
      case 0x3c: this.dr = this.bits(this.statusReg, 32); break;
      case 0xf0: this.dr = this.bits(0, 8); break;
      default: this.dr = new Array(256).fill(0);
    }
    this.drVal = [];
  }
  updateIr() {
    let v = 0; for (let i = 0; i < 8; i++) v |= this.irSh[i] << i;
    this.ir = v;
    if (v === 0x26) this.statusReg &= ~(1 << 9);                 // ISC_DISABLE
    if (v === 0x79) { this.statusReg |= 1 << 8; this.spiMode = false; this.stats.refresh++; }
  }
  updateDr() {
    let v = 0; for (let i = 0; i < 24 && i < this.drVal.length; i++) v |= this.drVal[i] << i;
    if (this.ir === 0xc6) this.statusReg |= 1 << 9;              // ISC_ENABLE
    if (this.ir === 0x0e) { this.statusReg &= ~(1 << 8); this.stats.sramCleared++; }
    if (this.ir === 0x3a && (v & 0xffff) === 0x68fe) this.spiMode = true;
  }

  // --- SPI フラッシュ ---
  spiStatus() { return (this.fstatus & ~0x03) | (this.wel ? 0x02 : 0); }
  csAssert() { this.spi = { cmd: null, k: 0, addr: 0, data: [] }; this.spiOut = 0; this.spiIn = 0; this.spiBit = 0; }
  csDeassert() {
    const s = this.spi; if (!s || s.cmd === null) return;
    if (s.cmd === 0x06) this.wel = 1;
    else if (s.cmd === 0x04) this.wel = 0;
    else if (s.cmd === 0x01) this.wel = 0;
    else if (s.cmd === 0xd8 || s.cmd === 0x20) {
      if (!this.wel) throw new Error('erase without WREN');
      const size = s.cmd === 0xd8 ? 0x10000 : 0x1000;
      const base = s.addr & ~(size - 1);
      this.flash.fill(0xff, base, base + size);
      this.wel = 0; this.stats.erased += size;
    } else if (s.cmd === 0x02) {
      if (!this.wel) throw new Error('program without WREN');
      if (s.data.length > 256) throw new Error(`page program too long: ${s.data.length}`);
      for (let i = 0; i < s.data.length; i++) {
        const a = (s.addr & ~0xff) | ((s.addr + i) & 0xff);      // ページ内ラップ
        this.flash[a] &= s.data[i];                              // NOR は 1->0 のみ
      }
      this.wel = 0; this.stats.programmed += s.data.length;
    }
    this.spi = null;
  }
  spiByte(b) {
    const s = this.spi;
    if (s.k === 0) s.cmd = b;
    else if (s.cmd === 0x01 && s.k === 1) this.fstatus = b;
    else if ((s.cmd === 0x02 || s.cmd === 0x03 || s.cmd === 0xd8 || s.cmd === 0x20) && s.k <= 3)
      s.addr = ((s.addr << 8) | b) >>> 0;
    else if (s.cmd === 0x02) s.data.push(b);
    s.k++;
    switch (s.cmd) {
      case 0x05: this.spiOut = this.spiStatus(); break;
      case 0x9f: this.spiOut = [0, 0xef, 0x40, 0x16][s.k] ?? 0; break;
      case 0x03: this.spiOut = s.k >= 4 ? this.flash[(s.addr + (s.k - 4)) % FLASH_SIZE] : 0; break;
      default: this.spiOut = 0;
    }
  }

  clock(tms, tdi) {
    let tdo = 0;
    if (this.state === 'SHIR') {
      tdo = this.irSh[0];
      this.irSh = this.irSh.slice(1); this.irSh.push(tdi);
    } else if (this.state === 'SHDR') {
      if (this.spiMode) {
        tdo = (this.spiOut >> (7 - this.spiBit)) & 1;            // SPI は MSB first
        this.spiIn = (this.spiIn << 1) | tdi;
        if (++this.spiBit === 8) { this.spiByte(this.spiIn & 0xff); this.spiIn = 0; this.spiBit = 0; }
      } else {
        tdo = this.dr.length ? this.dr[0] : 0;
        this.dr = this.dr.slice(1); this.dr.push(tdi);
        this.drVal.push(tdi);   // UPDATE-DR で見るのはシフト「イン」した値
      }
    }
    const next = NEXT[this.state][tms];
    if (next !== this.state) {
      if (this.state === 'SHDR' && this.spiMode) { this.csDeassert(); this.cs = false; }
      if (next === 'CAPDR' && !this.spiMode) this.captureDr();
      if (next === 'CAPIR') this.irSh = [1, 0, 0, 0, 0, 0, 0, 0];
      if (next === 'UPDR' && !this.spiMode) this.updateDr();
      if (next === 'UPIR') this.updateIr();
      if (next === 'SHDR' && this.spiMode) { this.csAssert(); this.cs = true; }
      if (next === 'TLR') { this.ir = 0xe0; this.spiMode = false; }
    }
    this.state = next;
    return tdo;
  }
}

// CH347 のパケットを解釈して Ecp5Sim を叩く偽 USB デバイス
class FakeCH347 {
  constructor(sim) {
    this.sim = sim; this.replies = []; this.prevTck = 0; this.lastTms = 0;
    this.manufacturerName = 'wch.cn'; this.productName = 'UART+SPI+I2C+JTAG';
    this.serialNumber = 'SIM'; this.productId = 0x55de; this.configuration = {};
    this.txBytes = 0; this.packets = 0; this.pending = null; this.splitIdx = 0;
  }
  async transferOut(ep, data) {
    if (ep !== 6) throw new Error('bad EP');
    this.txBytes += data.length; this.packets++;
    const cmd = data[0], n = data[1] | (data[2] << 8);
    if (cmd === 0xd0) { this.replies.push(Uint8Array.of(0xd0, 1, 0, 0)); return { status: 'ok' }; }
    if (cmd === 0xd1 || cmd === 0xd2) {
      const out = [];
      for (let i = 3; i < 3 + n; i++) {
        const v = data[i], tck = v & 1;
        if (tck && !this.prevTck) {
          this.lastTms = (v >> 1) & 1;
          out.push(this.sim.clock(this.lastTms, (v >> 4) & 1));
        }
        this.prevTck = tck;
      }
      if (cmd === 0xd2) this.replies.push(Uint8Array.from([0xd2, out.length & 0xff, out.length >> 8, ...out]));
      return { status: 'ok' };
    }
    if (cmd === 0xd3 || cmd === 0xd4) {
      const rx = new Uint8Array(n);
      for (let i = 0; i < n; i++) {
        let b = 0;
        for (let bit = 0; bit < 8; bit++) {
          b |= this.sim.clock(this.lastTms, (data[3 + i] >> bit) & 1) << bit;
        }
        rx[i] = b;
      }
      if (cmd === 0xd4) { const r = new Uint8Array(n + 3); r[0] = 0xd4; r[1] = n & 0xff; r[2] = n >> 8; r.set(rx, 3); this.replies.push(r); }
      return { status: 'ok' };
    }
    throw new Error(`unknown cmd 0x${cmd.toString(16)}`);
  }
  // ★実機の CH347 は長い応答を 1 パケットで返さず、クロックアウトしながら
  //   小分けに返してくる。1回で全部来る前提のコードは 509 バイト読み出しで
  //   壊れた (2026-09-04)。ここでも意図的に分割して、同じ前提が入り込んだら
  //   テストが落ちるようにしておく。
  async transferIn(ep, len) {
    if (!this.pending || this.pending.length === 0) {
      this.pending = this.replies.shift();
      if (!this.pending) throw new Error('応答キューが空です (ホスト側が余分に読もうとしている)');
    }
    let n = this.pending.length;
    if (n > 64) {   // 短いパケットはそのまま。長いものだけ分割する
      const SPLITS = [200, 64, 509, 1, 300];
      n = Math.min(n, SPLITS[this.splitIdx++ % SPLITS.length]);
    }
    const chunk = this.pending.subarray(0, n);
    this.pending = this.pending.subarray(n);
    if (chunk.length > len) throw new Error(`babble: 応答 ${chunk.length} > 要求 ${len}`);
    return { status: 'ok', data: new DataView(chunk.buffer, chunk.byteOffset, chunk.byteLength) };
  }
  async releaseInterface() {} async close() {}
}

// ビルド成果物が無い環境 (CI やクリーンな worktree) でも走れるように、
// parseLatticeBit が受け付ける最小の .bit を合成する。決定的にしたいので
// Math.random ではなく LCG を使う。
function syntheticBit(size, idcode = IDCODE) {
  const head = [0xff, 0x00];
  for (const line of ['Part: LFE5U-25F-6CABGA381', 'Date: 2026/09/04']) {
    for (const ch of line) head.push(ch.charCodeAt(0));
    head.push(0x00);
  }
  const body = [0xff, 0xff, 0xff, 0xbd, 0xb3,                       // ダミー3 + プリアンブル
    0xe2, 0x00, 0x00, 0x00,                                          // VERIFY_ID + 3バイト
    (idcode >>> 24) & 0xff, (idcode >>> 16) & 0xff, (idcode >>> 8) & 0xff, idcode & 0xff];
  let x = 0x12345678;
  while (body.length < size) { x = (Math.imul(x, 1103515245) + 12345) >>> 0; body.push((x >>> 16) & 0xff); }
  return Uint8Array.from([...head, ...body]);
}

// --- 実行 ---
const arg = process.argv[2];
const fallback = new URL('../../gateware/build/colorlight_i5/gateware/colorlight_i5.bit', import.meta.url).pathname;
let source, raw;
if (arg) { source = arg; raw = readFileSync(arg); }
else if (existsSync(fallback)) { source = fallback; raw = readFileSync(fallback); }
else { source = '合成ビットストリーム (200KB)'; raw = syntheticBit(200000); }
console.log(`対象: ${source}\n`);
const parsed = parseLatticeBit(raw);
const sim = new Ecp5Sim();
const jtag = new CH347Jtag();
jtag.device = new FakeCH347(sim);
jtag.intf = 4;

const logs = [];
const ecp5 = new Ecp5Flasher(jtag, (s) => logs.push(s));

let fail = 0;
const check = (name, cond, extra = '') => {
  console.log(`${cond ? '  OK  ' : '  NG  '} ${name}${extra ? ' — ' + extra : ''}`);
  if (!cond) fail++;
};

await jtag.setClock(3750000);
check('setClock', jtag.clockHz === 3750000, `${jtag.clockHz} Hz`);

await jtag.resetTap();
await jtag.gotoState('RTI');
const id = await ecp5.idcode();
check('IDCODE 読み出し', id === IDCODE, `0x${id.toString(16)}`);

const t0 = Date.now();
const phases = [];
await ecp5.program(parsed.data, {
  verify: true,
  onPhase: (p) => phases.push(p),
  onProgress: () => {},
});
const dt = ((Date.now() - t0) / 1000).toFixed(1);

check('フェーズ順', phases.join('>') === 'SPIモードへ移行>プロテクト解除>消去>書き込み>ベリファイ>プロテクト復帰>再コンフィグ', phases.join(' > '));
check('SRAM クリアが走った', sim.stats.sramCleared >= 1, `${sim.stats.sramCleared} 回`);
check('プログラム量', sim.stats.programmed === parsed.data.length, `${sim.stats.programmed} / ${parsed.data.length} bytes`);
check('消去量は 64KB 単位で必要十分',
  sim.stats.erased === Math.ceil(parsed.data.length / 0x10000) * 0x10000,
  `${sim.stats.erased} bytes`);
const written = sim.flash.subarray(0, parsed.data.length);
let diff = -1;
for (let i = 0; i < parsed.data.length; i++) if (written[i] !== parsed.data[i]) { diff = i; break; }
check('フラッシュ内容がビットストリームと一致', diff === -1, diff === -1 ? '' : `最初の不一致 0x${diff.toString(16)}`);
check('末尾以降は消去済みのまま', sim.flash[parsed.data.length] === 0xff);
check('ブロックプロテクトが元に戻っている', sim.fstatus === 0x1c, `0x${sim.fstatus.toString(16)}`);
check('REFRESH が発行された', sim.stats.refresh >= 1, `${sim.stats.refresh} 回`);
check('SPI モードを抜けている', sim.spiMode === false);
check('CS が開放されている', sim.cs === false);
check('TAP は Test-Logic-Reset', sim.state === 'TLR');

console.log(`\nUSB: ${jtag.device.packets.toLocaleString()} パケット / ` +
  `${(jtag.device.txBytes / 1024 / 1024).toFixed(2)} MB 送信, シミュレート ${dt}s`);
console.log('flasher ログ:'); for (const l of logs) console.log('  ' + l);
console.log(fail === 0 ? '\n全項目 OK' : `\n${fail} 件 NG`);
process.exit(fail ? 1 : 0);
