"""Command line entrypoint for yitrace-db."""
from __future__ import annotations

import argparse
from typing import Sequence


def _parse_bind(value: str) -> tuple[str, int]:
    if not value:
        raise argparse.ArgumentTypeError("bind must be host:port")
    host, sep, port_text = value.rpartition(":")
    if not sep or not host or not port_text:
        raise argparse.ArgumentTypeError("bind must be host:port")
    try:
        port = int(port_text)
    except ValueError as err:
        raise argparse.ArgumentTypeError("bind port must be an integer") from err
    if not (1 <= port <= 65535):
        raise argparse.ArgumentTypeError("bind port must be between 1 and 65535")
    return host, port


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="yitrace-db")
    sub = parser.add_subparsers(dest="command", required=True)
    serve = sub.add_parser("serve", help="serve an embedded yiTrace data dir over HTTP")
    serve.add_argument("--data-dir", required=True, help="yiTrace data directory")
    serve.add_argument("--bind", default="127.0.0.1:7878", help="host:port, default 127.0.0.1:7878")
    serve.add_argument("--host", help="override bind host")
    serve.add_argument("--port", type=int, help="override bind port")
    serve.add_argument("--tenant-id", help="default tenant id when X-Tenant-Id is missing")
    serve.add_argument("--workers", type=int, default=1, help="must stay 1 for embedded mode")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.command == "serve":
        if args.workers != 1:
            parser.error("embedded serve supports exactly one worker; use a single yiTrace server for multi-worker apps")
        host, port = _parse_bind(args.bind)
        if args.host:
            host = args.host
        if args.port:
            port = args.port
        try:
            import uvicorn
        except ImportError as err:
            raise SystemExit("serve requires: pip install 'yitrace-db[server]'") from err
        from .fastapi import create_yitrace_app

        app = create_yitrace_app(args.data_dir, tenant_id=args.tenant_id)
        uvicorn.run(app, host=host, port=port, workers=1)
        return 0
    parser.error("unknown command")
    return 2


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
