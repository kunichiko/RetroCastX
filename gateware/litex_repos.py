#!/usr/bin/env python3

# Git repositories ---------------------------------------------------------------------------------

# Get SHA1: git rev-parse HEAD

class GitRepo:
    def __init__(self, url, clone="regular", develop=True, editable=True, sha1=None, branch="master",
        tag=None):
        assert clone in ["regular", "recursive"]
        self.url      = url
        self.clone    = clone
        self.develop  = develop
        self.editable = editable
        self.sha1     = sha1
        self.branch   = branch
        self.tag      = tag


git_repos = {
    "migen": GitRepo(url="https://git.m-labs.hk/M-Labs/", clone="recursive", editable=False, sha1="4c2ae8dfeea37f235b52acb8166f12acaaae4f7c"),
    "pythondata-software-picolibc": GitRepo(url="https://github.com/litex-hub/", clone="recursive", sha1="6a13ccce7c575b32c102dd9dc52178505b81fe39"),
    "pythondata-software-compiler_rt": GitRepo(url="https://github.com/litex-hub/", sha1="6eb76609c9627bf26635e57c63fb22cda7115887"),
    "litex": GitRepo(url="https://github.com/enjoy-digital/", sha1="93c8d230e2e7e66fa823fb26d3f7fe5e442c72ee", tag=True),
    "liteiclink": GitRepo(url="https://github.com/enjoy-digital/", sha1="2da8e8becdc15037aa3947c8514cc4ee73ddf69f", tag=True),
    "liteeth": GitRepo(url="https://github.com/enjoy-digital/", sha1="276c9e37fb4d92a5c0f30d39a51c20f42a59cf93", tag=True),
    "litedram": GitRepo(url="https://github.com/enjoy-digital/", sha1="1b7cc79add189989fb5604a93f59a9837eaa24b8", tag=True),
    "litepcie": GitRepo(url="https://github.com/enjoy-digital/", sha1="e84e0b9eea85cc0704775b71a922e2baeead4353", tag=True),
    "litesata": GitRepo(url="https://github.com/enjoy-digital/", sha1="fab17deecb29b88d51e9f6252bec67f2e1e09fe4", tag=True),
    "litesdcard": GitRepo(url="https://github.com/enjoy-digital/", sha1="227d61bc2b92ca56cac78a539b98e378468b1ba1", tag=True),
    "litescope": GitRepo(url="https://github.com/enjoy-digital/", sha1="6bf3b92f261c50b8c7c74947f84e692ae846f512", tag=True),
    "litejesd204b": GitRepo(url="https://github.com/enjoy-digital/", sha1="84734f7f63ecd5e27b0cdd0d175cfcfe521c2bfa", tag=True),
    "litedsp": GitRepo(url="https://github.com/enjoy-digital/", sha1="8babdd4e1806f5b52f8d5882cfa2ee458b476dbf", branch="main"),
    "litespi": GitRepo(url="https://github.com/litex-hub/", sha1="8ca711b8e5c705c9502d6e8d162ae13f7c2fb128", tag=True),
    "litei2c": GitRepo(url="https://github.com/litex-hub/", sha1="81cf75d3e6fc8ddfe4dece68cd7e2b39a2a4385e", branch="main", tag=True),
    "litex-boards": GitRepo(url="https://github.com/litex-hub/", sha1="2f06d08a35e3c8063880adfe8d602513358b2564", tag=True),
    "pythondata-misc-tapcfg": GitRepo(url="https://github.com/litex-hub/", sha1="a12c3f592c99f9c082fdc68c065b81cbd6e6b238"),
    "pythondata-misc-usb_ohci": GitRepo(url="https://github.com/litex-hub/", clone="recursive", sha1="17c1d3d6548ea267e19aec3cb6d2e64335a1bb2a"),
    "pythondata-cpu-lm32": GitRepo(url="https://github.com/litex-hub/", sha1="0f1d1b91202b95a9b749a745848430d64afb4400"),
    "pythondata-cpu-mor1kx": GitRepo(url="https://github.com/litex-hub/", sha1="ba6ea16dc1250ac138ea0d923af4f91da790892f"),
    "pythondata-cpu-minerva": GitRepo(url="https://github.com/litex-hub/", sha1="ab328774891c70694c6576be19d1d3e427d9f435"),
    "pythondata-cpu-naxriscv": GitRepo(url="https://github.com/litex-hub/", sha1="20da269306bb3bfabd09de08d4c1be1fbc202474"),
    "pythondata-cpu-sentinel": GitRepo(url="https://github.com/litex-hub/", sha1="7ec5e1e5db1a53910e4c58ae4c098ebce3f9591f", branch="main"),
    "pythondata-cpu-serv": GitRepo(url="https://github.com/litex-hub/", sha1="111947d7ab652c28642d7ff0a528dae293ca4601"),
    "pythondata-cpu-vexiiriscv": GitRepo(url="https://github.com/litex-hub/", sha1="15cfab529a17c473d0fc75edf3f409eb374cef35", branch="main"),
    "pythondata-cpu-vexriscv": GitRepo(url="https://github.com/litex-hub/", sha1="642ecfed1c84460555d6d803d660cc60cfc1ecb6"),
    "pythondata-cpu-vexriscv-smp": GitRepo(url="https://github.com/litex-hub/", clone="recursive", sha1="217d23d7e9ad5556c17a73dc6ffc1971765f3d7c"),
}

# Installs -----------------------------------------------------------------------------------------

frozen_repos = ['migen', 'pythondata-software-picolibc', 'pythondata-software-compiler_rt', 'litex', 'liteiclink', 'liteeth', 'litedram', 'litepcie', 'litesata', 'litesdcard', 'litescope', 'litejesd204b', 'litedsp', 'litespi', 'litei2c', 'litex-boards', 'pythondata-misc-tapcfg', 'pythondata-misc-usb_ohci', 'pythondata-cpu-lm32', 'pythondata-cpu-mor1kx', 'pythondata-cpu-minerva', 'pythondata-cpu-naxriscv', 'pythondata-cpu-sentinel', 'pythondata-cpu-serv', 'pythondata-cpu-vexiiriscv', 'pythondata-cpu-vexriscv', 'pythondata-cpu-vexriscv-smp']

# Reuse the frozen set for every install config.
minimal_repos  = frozen_repos
standard_repos = frozen_repos
full_repos     = frozen_repos

install_configs = {
    "minimal"  : minimal_repos,
    "standard" : standard_repos,
    "full"     : full_repos,
}
