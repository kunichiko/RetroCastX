#!/usr/bin/env python3
"""presentedTime の生ログから、パネルのリフレッシュ格子周期を推定する。

- 隣接差分のヒストグラム
- 候補周期 T について「差分を T で割った余りの格子からのズレ」を最小化する T を探す
  (T=8.333ms付近で鋭い最小 → 120Hz固定。T=18.03ms付近が最小 → ソース追従)
"""
import sys
import numpy as np

path = sys.argv[1]
t = np.loadtxt(path)
d = np.diff(t)
d = d[(d > 0.5) & (d < 100)]  # 外れ値除去
print(f"n={len(d)}  mean={d.mean():.3f}ms  σ={d.std():.3f}  → {1000/d.mean():.2f}Hz")

print("\n-- 差分ヒストグラム (0.5ms bin, 上位12) --")
hist, edges = np.histogram(d, bins=np.arange(0, 45, 0.5))
for i in np.argsort(hist)[::-1][:12]:
    if hist[i] == 0:
        continue
    print(f"  {edges[i]:5.1f}-{edges[i+1]:5.1f}ms : {hist[i]:4d}  {'#' * (hist[i] * 40 // hist.max())}")

print("\n-- 格子周期の探索: 残差 = mean(min(d mod T, T - d mod T)) --")


def residual(T):
    r = np.mod(d, T)
    return np.minimum(r, T - r).mean()


Ts = np.arange(4.0, 30.0, 0.005)
res = np.array([residual(T) for T in Ts])
# ランダム(格子なし)なら残差 ≈ T/4。それに対する比で「格子らしさ」を見る
score = res / (Ts / 4)
best = np.argsort(score)[:400]
shown = []
for i in best:
    if any(abs(Ts[i] - s) < 0.4 for s in shown):
        continue
    shown.append(Ts[i])
    print(f"  T={Ts[i]:6.3f}ms ({1000/Ts[i]:7.2f}Hz)  残差={res[i]:.3f}ms  ランダム比={score[i]:.2f}")
    if len(shown) >= 6:
        break
print(f"\n  参考: T=8.333ms(120Hz) 残差={residual(8.3333):.3f}ms  ランダム比={residual(8.3333)/(8.3333/4):.2f}")
