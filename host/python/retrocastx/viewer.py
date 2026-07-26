"""Live viewer (optional; requires pygame: pip install pygame).

Usage:
    python3 -m retrocastx.viewer [--port 34600] [--scale 0]   # scale 0 = auto integer scale
"""
import argparse
import socket
import sys

from . import protocol as proto
from .receiver import FrameAssembler


def main():
    try:
        import pygame
    except ImportError:
        print("viewer requires pygame:  python3 -m pip install pygame", file=sys.stderr)
        sys.exit(1)

    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--bind", default="0.0.0.0")
    ap.add_argument("--port", type=int, default=proto.DEFAULT_PORT)
    ap.add_argument("--scale", type=int, default=0, help="integer scale (0 = auto)")
    args = ap.parse_args()

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
    sock.bind((args.bind, args.port))
    sock.setblocking(False)

    pygame.init()
    screen = None
    scale = args.scale
    asm = FrameAssembler()
    clock = pygame.time.Clock()
    print("viewer listening on %s:%d" % (args.bind, args.port))

    running = True
    while running:
        for event in pygame.event.get():
            if event.type == pygame.QUIT or (
                    event.type == pygame.KEYDOWN and event.key == pygame.K_ESCAPE):
                running = False
        latest = None
        while True:  # drain everything pending, keep only the newest frame
            try:
                datagram, _ = sock.recvfrom(65535)
            except BlockingIOError:
                break
            for _, img, _ in asm.feed(datagram):
                latest = img
        if latest is not None:
            h, w = latest.shape[:2]
            if screen is None:
                if scale <= 0:
                    scale = max(1, min(1280 // w, 960 // h))
                screen = pygame.display.set_mode((w * scale, h * scale))
                pygame.display.set_caption("RetroCastX %dx%d x%d" % (w, h, scale))
            surf = pygame.surfarray.make_surface(latest.swapaxes(0, 1))
            if scale != 1:
                surf = pygame.transform.scale(surf, (w * scale, h * scale))
            screen.blit(surf, (0, 0))
            pygame.display.flip()
        clock.tick(120)
    pygame.quit()


if __name__ == "__main__":
    main()
