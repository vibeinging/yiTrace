"""yitrace SDK command line helpers."""
from __future__ import annotations

import argparse
import time
from typing import Sequence

from .client import connect
from .exporter import SpoolConsumer


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="yitrace")
    sub = parser.add_subparsers(dest="command", required=True)

    consume = sub.add_parser("consume-spool", help="consume local spool files into an embedded yiTrace DB")
    consume.add_argument("--data-dir", required=True, help="embedded yiTrace data directory")
    consume.add_argument("--spool-dir", required=True, help="spool directory written by SpoolDbExporter")
    consume.add_argument("--tenant-id", help="default tenant id when spool files do not carry one")
    consume.add_argument("--interval", type=float, default=0.5, help="sleep seconds between polling rounds")
    consume.add_argument("--limit", type=int, help="max events to consume per polling round")
    consume.add_argument("--once", action="store_true", help="consume one round then exit")
    consume.add_argument("--keep-done", action="store_true", help="move consumed files to done/ instead of deleting them")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.command == "consume-spool":
        db = connect(path=args.data_dir, tenant_id=args.tenant_id)
        try:
            consumer = SpoolConsumer(db, args.spool_dir, tenant_id=args.tenant_id, keep_done=args.keep_done)
            while True:
                consumed = consumer.consume_once(limit=args.limit)
                if args.once:
                    print(f"consumed={consumed}")
                    return 0
                if consumed == 0:
                    time.sleep(args.interval)
        finally:
            db.close()
    parser.error("unknown command")
    return 2


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
