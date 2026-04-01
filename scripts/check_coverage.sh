#!/bin/bash
if [ -z "$1" ]; then
    echo "::error::Failed to parse coverage"
    exit 1
fi
coverage=$1
threshold=70.0
python3 -c "
import sys
c = float('$coverage')
if c < $threshold:
    print(f'::error::Coverage {c:.2f}% is below {threshold:.2f}% threshold')
    sys.exit(1)
print(f'Coverage check passed: {c:.2f}% >= {threshold:.2f}%')
"
