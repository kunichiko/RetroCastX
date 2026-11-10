#!/usr/bin/env python3
"""個体設定ページ(EEPROM 0x00..0x1F)の読み書きを検証(実機不要)。

24AA025E48 を**書込みまで含めて**模擬する。ここで手を抜くと意味が無いので、
実物と同じ意地悪をさせる:
- ページ書込みは8バイト境界で**折り返す**(超えた分は先頭を上書き)。
  → 16バイトずつ書くコードはここで壊れる
- 書込み後は最大5ms相当のあいだ**アドレスにACKを返さない**(書込み中)。
  → ACKポーリングを省いた実装はここで次のページを落とす
- EUI-48(0xFA..0xFF)は書込み不可

検証項目:
1. 妥当な設定ページがあれば name/静的IP/flags を読み込む(cfg_valid=1)
2. チェックサムを1バイト壊すと**採用しない**(cfg_err=2、既定名のまま)
3. magic が無ければ「未設定」(cfg_err=1)
4. set_stb で書き換え → save_stb で EEPROM へ焼け、読み直しても同じ値になる
5. 焼く順序が**ヘッダ(ページ0)を最後**にしている(commit-last)
6. 焼いた内容で起動し直すと採用される
7. LED の欄だけ焼く(save_sel=1)と、名前・IP は**EEPROM にあった値のまま**になる
   (動作中の未保存の試しを巻き込まない)
8. 未設定の EEPROM に LED の欄だけ焼くと、既定の名前で妥当なページができる
9. EEPROM が2個(0x50 / 0x51)あれば、ポートごとの EUI-48 を両方読む。
   名乗り(mac)は 0x50 の方
10. 0x51 しか無い基板では、名乗りも設定ページも 0x51 から読む
"""
import sys, os
sys.path.insert(0, os.path.dirname(__file__))
from migen import *
from retrocastx_i2c import (StatusDisplay, DEFAULT_BOARD_NAME, CFG_PAGE_MAGIC,
                            CFG_PAGE_VER, CFG_PAGE_LEN, CFG_WR_PAGE,
                            CFG_FLAG_STATIC_IP)

EUI = [0x74, 0xC9, 0x0F, 0x7C, 0x84, 0x29]
EUI1 = [0x74, 0xC9, 0x0F, 0x7C, 0x84, 0x2A]     # 0x51(LAN1)側
# 書込み中を模す長さ[ポーリング回数]。0なら即座にACK
BUSY_POLLS = 3


def make_page(name, ip=(0, 0, 0, 0), flags=0, ver=CFG_PAGE_VER, magic=CFG_PAGE_MAGIC,
              led_idle=0):
    b = bytearray(CFG_PAGE_LEN)
    b[0], b[1] = magic
    b[2] = ver
    b[3] = flags
    b[4:8] = bytes(ip)
    b[9:16] = bytes(7)
    b[9] = led_idle
    b[16:32] = name.encode()[:16].ljust(16, b"\0")
    b[8] = 0
    b[8] = (-sum(b)) & 0xFF
    assert sum(b) & 0xFF == 0
    return bytes(b)


class Eeprom24AA02:
    """I2C スレーブ(既定 0x50)。open-drainバスのビットを直接叩く。"""

    def __init__(self, mem=None, addr=0x50, eui=EUI):
        self.i2c_addr = addr
        self.mem = bytearray(256)
        self.mem[0:] = bytes([0xFF]) * 256
        if mem:
            self.mem[0:len(mem)] = mem
        self.mem[0xFA:0x100] = bytes(eui) + bytes(2)
        self.scl = 1
        self.line = 1
        self.slave_low = 0
        self.txn = False
        self.nbit = -1
        self.shift = 0
        self.mode = 'ADDR'
        self.addr = None
        self.ptr = 0
        self.got_ptr = False
        self.rd_byte = 0
        # 読出しアドレスに対する**自分のACK**を、マスタのACKと取り違えない
        self.skip_ack = False
        self.busy = 0           # >0 のあいだアドレスにNACKを返す
        self.wr_buf = {}        # このトランザクションで書かれたバイト
        self.writes = []        # [(先頭アドレス, [バイト...])] 焼いた順
        # 自分以外(OLED 0x3C / TVP 0x5C)が呼ばれた = 起動時の読み出しが終わった
        self.saw_other = False

    # --- 内部 ---
    def _start(self):
        self.txn = True
        self.nbit = -1
        self.shift = 0
        self.mode = 'ADDR'
        self.addr = None
        self.slave_low = 0
        self.got_ptr = False
        self.wr_buf = {}

    def _stop(self):
        if self.wr_buf:
            base = min(self.wr_buf)
            self.writes.append((base, [self.wr_buf[a] for a in sorted(self.wr_buf)]))
            for a, v in self.wr_buf.items():
                if a < 0xF8:            # EUI-48 はハードウェア保護
                    self.mem[a] = v
            self.busy = BUSY_POLLS
        self.txn = False
        self.slave_low = 0
        self.wr_buf = {}

    def _accept(self, byte):
        """アドレスバイトを受けた。ACKするなら True。"""
        if byte >> 1 != self.i2c_addr:
            # 0x50〜0x53 は EEPROM 同士(相方)。それ以外が呼ばれたら起動時の
            # 読み出しが終わった
            if not 0x50 <= byte >> 1 <= 0x53:
                self.saw_other = True
            # ★**自分宛でなければ STOP まで黙る。** ADDR のままだと、続く
            #   レジスタ番地(0xFA など)をアドレスと取り違えて「他所が呼ばれた」
            #   と誤判定する。相方の EEPROM が読まれている最中に起きる
            self.mode = 'IDLE'
            return False
        if self.busy:               # 書込み中: アドレスにも応答しない
            self.busy -= 1
            self.mode = 'IDLE'
            return False
        self.addr = byte >> 1
        if byte & 1:
            self.mode = 'READ'
            self.rd_byte = self.mem[self.ptr]
            self.skip_ack = True
        else:
            self.mode = 'WRITE'
        return True

    def _wr_data(self, byte):
        if not self.got_ptr:
            self.ptr = byte
            self.got_ptr = True
            return
        # ★ページ内で折り返す。8バイトを超えて書くと先頭が潰れる
        page = self.ptr & ~(CFG_WR_PAGE - 1)
        self.wr_buf[self.ptr] = byte
        self.ptr = page | ((self.ptr + 1) & (CFG_WR_PAGE - 1))

    def step(self, scl, m_sda):
        line = 0 if (m_sda == 0 or self.slave_low) else 1
        pscl, pline = self.scl, self.line
        if scl == 1 and pscl == 1:
            if pline == 1 and line == 0:
                self._start()
            elif pline == 0 and line == 1:
                self._stop()
        if pscl == 0 and scl == 1 and self.txn:
            self.nbit += 1
            pos = self.nbit % 9
            if pos < 8 and self.mode in ('ADDR', 'WRITE'):
                self.shift = ((self.shift << 1) | line) & 0xFF
            elif pos == 8 and self.mode == 'READ':
                if self.skip_ack:                 # 直前は自分が返したACK
                    self.skip_ack = False
                elif line == 0:                   # マスタがACK → 次のバイトへ
                    self.ptr = (self.ptr + 1) & 0xFF
                    self.rd_byte = self.mem[self.ptr]
                else:
                    self.mode = 'IDLE'
        if pscl == 1 and scl == 0 and self.txn:
            npos = (self.nbit + 1) % 9
            self.slave_low = 0
            if npos == 8:
                if self.mode == 'ADDR':
                    if self._accept(self.shift):
                        self.slave_low = 1
                    self.shift = 0
                elif self.mode == 'WRITE':
                    self._wr_data(self.shift)
                    self.shift = 0
                    self.slave_low = 1
            elif npos < 8 and self.mode == 'READ':
                self.slave_low = 1 if not ((self.rd_byte >> (7 - npos)) & 1) else 0
        self.scl, self.line = scl, line
        return line


def _name_of(v):
    return v.to_bytes(16, "little").split(b"\0")[0].decode()


def run(mem, steps=50000, actions=None, until=None, eeproms=None):
    """actions: [(何ステップ目で, ジェネレータ)] を流し込む。

    ★**打ち切り条件を渡せるようにしてある。** OLEDの1フレーム(1024バイト)
      だけで7万サイクル以上あるので、固定ステップ数で回すと本題(設定ページの
      読み書き)に対して桁違いに待たされる。CIで回せる時間に収めるため、
      見たいことが起きたら止める。
    """
    """eeproms: バスに載せる EEPROM の並び。省略時は 0x50 の1個(mem を持つ)。"""
    dut = StatusDisplay(pads=None, sys_clk_freq=4000, i2c_freq=1000)
    ees = eeproms if eeproms is not None else [Eeprom24AA02(mem)]
    ee = ees[0]
    res = {}

    def bus_step(scl, m_sda):
        # open-drain: 誰か1つでも引けば Low
        return min(e.step(scl, m_sda) for e in ees)

    def tb():
        pending = list(actions or [])
        hit = False
        for i in range(steps):
            scl = 0 if (yield dut.scl_low) else 1
            m_sda = 0 if (yield dut.sda_low) else 1
            yield dut.m.sda_in.eq(bus_step(scl, m_sda))
            while pending and pending[0][0] == i:
                _, gen = pending.pop(0)
                yield from gen(dut)
            if until is not None and not pending:
                done = yield from until(dut, ee)
                if done:
                    hit = True
                    break
            yield
        res['steps'] = i
        res['hit'] = hit or until is None
        res['mac'] = (yield dut.mac)
        res['mac_valid'] = (yield dut.mac_valid)
        res['mac_alt'] = (yield dut.mac_alt)
        res['eui0'] = (yield dut.eui[0])
        res['eui1'] = (yield dut.eui[1])
        res['eui_valid'] = (yield dut.eui_valid)
        res['name'] = _name_of((yield dut.cfg_name))
        res['ip'] = (yield dut.cfg_ip)
        res['flags'] = (yield dut.cfg_flags)
        res['led_idle'] = (yield dut.cfg_led_idle)
        res['valid'] = (yield dut.cfg_valid)
        res['err'] = (yield dut.cfg_err)
        res['save'] = (yield dut.save_state)

    run_simulation(dut, tb())
    assert res['hit'], "打ち切り条件に届かなかった(steps=%d)" % res['steps']
    res['writes'] = ee.writes
    res['mem'] = bytes(ee.mem)
    return res


def boot_read_done(dut, ee):
    """起動時の EEPROM 読み出しが済んだ(次のスレーブへ移った)。"""
    yield
    return ee.saw_other


def save_done(dut, ee):
    st = yield dut.save_state
    return st in (2, 3)


def ip_int(o):
    return (o[0] << 24) | (o[1] << 16) | (o[2] << 8) | o[3]


def main():
    # --- 1) 妥当なページを読む ---
    page = make_page("x68k-desk", ip=(192, 168, 10, 77), flags=CFG_FLAG_STATIC_IP,
                     led_idle=1)
    r = run(page, until=boot_read_done)
    print(f"1) name={r['name']!r} ip={r['ip']:#010x} flags={r['flags']:#04x} "
          f"valid={r['valid']} err={r['err']} mac={r['mac']:#014x}")
    assert r['mac_valid'] == 1 and r['mac'] == int.from_bytes(bytes(EUI), "big"), "EUI-48が読めていない"
    assert r['valid'] == 1 and r['err'] == 0, f"設定ページを採用していない err={r['err']}"
    assert r['name'] == "x68k-desk", r['name']
    assert r['ip'] == ip_int((192, 168, 10, 77)), f"{r['ip']:#x}"
    assert r['flags'] == CFG_FLAG_STATIC_IP
    assert r['led_idle'] == 1, f"LEDの切断中の動作を読んでいない: {r['led_idle']}"

    # --- 2) チェックサムが壊れていたら採用しない ---
    bad = bytearray(page)
    bad[20] ^= 0x01                      # name の途中を1ビット壊す
    r = run(bytes(bad), until=boot_read_done)
    print(f"2) name={r['name']!r} valid={r['valid']} err={r['err']}")
    assert r['valid'] == 0 and r['err'] == 2, f"壊れたページを採用した err={r['err']}"
    assert r['name'] == DEFAULT_BOARD_NAME, r['name']

    # --- 3) 未設定(空のEEPROM) ---
    r = run(None, until=boot_read_done)
    print(f"3) name={r['name']!r} valid={r['valid']} err={r['err']}")
    assert r['valid'] == 0 and r['err'] == 1, f"err={r['err']}"
    assert r['name'] == DEFAULT_BOARD_NAME

    # --- 4) 書き換えて焼く ---
    NEW = "pc98-rack"
    NEW_IP = (10, 0, 0, 42)
    nb = NEW.encode().ljust(16, b"\0")

    def do_set(dut):
        for sel, val in (
            (0, int.from_bytes(nb[0:4], "little")),
            (1, int.from_bytes(nb[4:8], "little")),
            (2, int.from_bytes(nb[8:12], "little")),
            (3, int.from_bytes(nb[12:16], "little")),
            (4, ip_int(NEW_IP)),
            (5, CFG_FLAG_STATIC_IP),
            (6, 2),                      # LED: 切断中は消灯
        ):
            yield dut.set_sel.eq(sel)
            yield dut.set_val.eq(val)
            yield dut.set_stb.eq(1)
            yield
            yield dut.set_stb.eq(0)
            yield
        yield dut.save_stb.eq(1)
        yield
        yield dut.save_stb.eq(0)

    r = run(page, steps=400000, actions=[(3000, do_set)], until=save_done)
    print(f"4) name={r['name']!r} ip={r['ip']:#010x} save_state={r['save']} "
          f"writes={[(hex(a), len(v)) for a, v in r['writes']]}")
    assert r['save'] == 2, f"保存に失敗している save_state={r['save']}"
    assert r['name'] == NEW and r['ip'] == ip_int(NEW_IP)

    # 焼けた中身が読み直せる形になっているか(=次回の起動で採用される)
    got = r['mem'][:CFG_PAGE_LEN]
    assert sum(got) & 0xFF == 0, "チェックサムが合っていない"
    assert got[:2] == bytes(CFG_PAGE_MAGIC) and got[2] == CFG_PAGE_VER
    assert got[4:8] == bytes(NEW_IP), got[4:8]
    assert got[9] == 2, f"LEDの切断中の動作が焼けていない: {got[9]}"
    assert got[16:32] == nb, got[16:32]

    # --- 5) 焼く順序: ヘッダ(0x00)が最後 ---
    bases = [a for a, _ in r['writes']]
    print(f"5) 焼いた順: {[hex(a) for a in bases]}")
    assert all(len(v) <= CFG_WR_PAGE for _, v in r['writes']), "8バイトを超えて書いている"
    assert bases[-1] == 0x00, f"ヘッダを最後に書いていない: {[hex(a) for a in bases]}"
    assert sorted(bases) == [0x00, 0x08, 0x10, 0x18], bases

    # --- 6) 焼いた内容で起動し直すと採用される ---
    r2 = run(r['mem'][:CFG_PAGE_LEN], until=boot_read_done)
    print(f"6) 再起動後 name={r2['name']!r} ip={r2['ip']:#010x} valid={r2['valid']}")
    assert r2['valid'] == 1 and r2['name'] == NEW and r2['ip'] == ip_int(NEW_IP)
    assert r2['led_idle'] == 2, r2['led_idle']

    # --- 7) LED の欄だけ焼く: 名前・IP の未保存の試しを巻き込まない ---
    TRIAL = "trial-name".encode().ljust(16, b"\0")

    def do_led_only(dut):
        for sel, val in [(k, int.from_bytes(TRIAL[4 * k:4 * k + 4], "little")) for k in range(4)] \
                + [(4, ip_int((172, 16, 0, 9))), (6, 1)]:
            yield dut.set_sel.eq(sel)
            yield dut.set_val.eq(val)
            yield dut.set_stb.eq(1)
            yield
            yield dut.set_stb.eq(0)
            yield
        yield dut.save_sel.eq(1)
        yield dut.save_stb.eq(1)
        yield
        yield dut.save_stb.eq(0)
        yield dut.save_sel.eq(0)

    # 起動時の設定ページの読み込み(3000〜6000 サイクルの間)が済んでから書き換える。
    # 済む前だと「EEPROM にある値」がまだ既定のままで、試験にならない
    r = run(page, steps=400000, actions=[(8000, do_led_only)], until=save_done)
    got = r['mem'][:CFG_PAGE_LEN]
    print(f"7) LEDだけ焼く: 焼いた名前={_name_of(int.from_bytes(got[16:32], 'little'))!r} "
          f"led_idle={got[9]} / 動作中の名前={r['name']!r}")
    assert r['save'] == 2, f"保存に失敗している save_state={r['save']}"
    assert sum(got) & 0xFF == 0, "チェックサムが合っていない"
    assert got[9] == 1, f"LEDの欄が焼けていない: {got[9]}"
    assert got[16:32] == page[16:32], "未保存の名前まで焼いた"
    assert got[4:8] == page[4:8] and got[3] == page[3], "未保存のIPまで焼いた"
    assert r['name'] == "trial-name", "動作中の値が戻ってしまった"
    r3 = run(got, until=boot_read_done)
    assert r3['valid'] == 1 and r3['name'] == "x68k-desk" and r3['led_idle'] == 1

    # --- 8) 未設定の EEPROM に LED の欄だけ焼く ---
    r = run(None, steps=400000, actions=[(3000, do_led_only)], until=save_done)
    r4 = run(r['mem'][:CFG_PAGE_LEN], until=boot_read_done)
    print(f"8) 未設定から: 再起動後 name={r4['name']!r} ip={r4['ip']:#x} "
          f"led_idle={r4['led_idle']} valid={r4['valid']}")
    assert r['save'] == 2
    assert r4['valid'] == 1 and r4['name'] == DEFAULT_BOARD_NAME and r4['ip'] == 0 \
        and r4['led_idle'] == 1

    # --- 9) EEPROM が2個: ポートごとの EUI-48 を両方読む ---
    e48 = lambda b: int.from_bytes(bytes(b), "big")
    r = run(None, until=boot_read_done,
            eeproms=[Eeprom24AA02(page), Eeprom24AA02(None, 0x51, EUI1)])
    print(f"9) 2個: eui0={r['eui0']:#014x} eui1={r['eui1']:#014x} "
          f"valid={r['eui_valid']:#04b} mac={r['mac']:#014x} alt={r['mac_alt']}")
    assert r['eui_valid'] == 0b11, f"両方読めていない {r['eui_valid']:#b}"
    assert r['eui0'] == e48(EUI) and r['eui1'] == e48(EUI1)
    assert r['mac_valid'] == 1 and r['mac'] == e48(EUI) and r['mac_alt'] == 0, \
        "名乗りが 0x50 の EUI-48 になっていない"
    assert r['valid'] == 1 and r['name'] == "x68k-desk", "設定ページを 0x50 から読んでいない"

    # --- 10) 0x51 しか無い ---
    r = run(None, until=boot_read_done,
            eeproms=[Eeprom24AA02(page, 0x51, EUI1)])
    print(f"10) 0x51のみ: eui1={r['eui1']:#014x} valid={r['eui_valid']:#04b} "
          f"mac={r['mac']:#014x} alt={r['mac_alt']} name={r['name']!r}")
    assert r['eui_valid'] == 0b10
    assert r['mac_valid'] == 1 and r['mac'] == e48(EUI1) and r['mac_alt'] == 1
    assert r['valid'] == 1 and r['name'] == "x68k-desk", "設定ページを 0x51 から読んでいない"

    print("\n[OK] 設定ページ: 読み込み/破損検出/未設定/書込み(8バイトページ+ACKポーリング)"
          "/commit-last/焼き直し後の再読込/LEDの欄だけ焼く/EUI-48 を2個とも読む を確認")


if __name__ == "__main__":
    main()
