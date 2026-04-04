#!/usr/bin/env bash
set -euo pipefail

case "$(uname -s)" in
    Linux) exec "$(dirname "$0")/package-linux.sh" ;;
    Darwin) exec "$(dirname "$0")/package-macos.sh" ;;
    *)
        echo "unsupported host for package.sh; use scripts/package.ps1 on Windows" >&2
        exit 1
        ;;
esac
