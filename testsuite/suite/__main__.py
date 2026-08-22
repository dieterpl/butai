"""Entry point: `python3 -m suite [profile] [options]`.

It lives here rather than in `runner.py` on purpose. Running `python3 -m
suite.runner` would load that module twice — once as `__main__`, once as
`suite.runner` when a test file imports the `@test` decorator — giving the
decorator a different `REGISTRY` than the runner reads, and a silent run of
zero tests.
"""

import sys

from .runner import main

if __name__ == "__main__":
    sys.exit(main())
