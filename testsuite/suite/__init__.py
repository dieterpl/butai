"""butai Docker test suite — support library.

Standard library only, matching `examples/api-client.py` and `web/server.py`:
the daemon's API is meant to be reachable from anything that can open a Unix
socket, so a suite that needed a client package would be assuming away the
thing it is supposed to prove.
"""

__all__ = [
    "butai",
    "coverage",
    "daemon",
    "fixtures",
    "metrics",
    "msgpack",
    "report",
    "runner",
    "screen",
]
