#!/usr/bin/env bash
# Sum every "test result: ok. N passed" line across the workspace.
set -uo pipefail
cd /mnt/d/4
bash scripts/cargo.sh test --workspace 2>&1 | grep -E 'test result:' > /tmp/ws-test.txt
python3 - <<'PY'
import re
total = 0
failed = 0
with open('/tmp/ws-test.txt') as fh:
    for line in fh:
        m = re.search(r'(\d+) passed; (\d+) failed', line)
        if m:
            total += int(m.group(1))
            failed += int(m.group(2))
print(f"passed={total} failed={failed}")
PY
