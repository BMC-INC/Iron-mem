# Native Windows uses MCP; Bash lifecycle hooks are installed on Unix only.
$ErrorActionPreference = "Stop"
if (-not (Get-Command python -ErrorAction SilentlyContinue)) {
    throw "Python 3.9+ is required. Install Python, then rerun this installer."
}
& python (Join-Path $PSScriptRoot "scripts/install_local.py") --source $PSScriptRoot @args
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
