#!/usr/bin/env python3
"""atopile部品APIが停止中(#1829)でも、既知のLCSC番号から部品を
ローカル取り込みする(`ato create part` のオフライン代替)。

atopileの検索API(components.atopileapi.com)は死んでいるが、フットプリント
実体はEasyEDA(easyeda.com、生存)から取得できる。`ato create part` は
検索でAPIを叩くため失敗するので、検索を飛ばしてEasyEDA取得+ローカル取り込みを
直接呼ぶ。取得後のメーカー名enrich(atopileapi)だけスタブ化する。

使い方(atopile同梱のpythonで、プロジェクトディレクトリから):
    /Users/ohnaka/.local/share/uv/tools/atopile/bin/python tools/ingest_parts.py C10429 C3824085 ...

生成物: parts/<MFR_PN>/{*.ato,*.kicad_mod,*.kicad_sym} と import文の一覧。
"""
import sys

import faebryk.libs.picker.api.api as apimod


class _StubClient:
    # atopileapi(死亡)へのメーカー名問い合わせをスキップさせる
    def fetch_part_by_lcsc(self, *a, **k):
        return []


apimod.get_api_client = lambda *a, **k: _StubClient()

from atopile.config import config  # noqa: E402

config.apply_options(entry="main.ato:App")

from faebryk.libs.picker.lcsc import download_easyeda_info  # noqa: E402
from faebryk.libs.part_lifecycle import PartLifecycle  # noqa: E402


def main(lcsc_ids):
    life = PartLifecycle.singleton()
    src = config.project.paths.src
    imports = []
    for lcsc in lcsc_ids:
        try:
            epart = download_easyeda_info(lcsc, get_model=True)
            apart = life.library.ingest_part_from_easyeda(epart)
            stmt = apart.generate_import_statement(src)
            imports.append((lcsc, apart.identifier, stmt))
            print(f"OK  {lcsc:12} -> {apart.identifier}")
        except Exception as e:
            print(f"ERR {lcsc:12} -> {type(e).__name__}: {e}")
    print("\n# --- import statements ---")
    for lcsc, ident, stmt in imports:
        print(f"{stmt}   # {lcsc}")


if __name__ == "__main__":
    main(sys.argv[1:])
