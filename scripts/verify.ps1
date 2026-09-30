param([switch]$Package)
$ErrorActionPreference = 'Stop'
function Run-Check([string]$Program,[string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}
Run-Check 'pnpm' @('typecheck')
Run-Check 'pnpm' @('lint')
Run-Check 'pnpm' @('test')
Run-Check 'node' @('scripts/check-style.mjs')
Run-Check 'cargo' @('fmt','--all','--','--check')
Run-Check 'cargo' @('test','-p','astraforge-core')
Run-Check 'cargo' @('clippy','-p','astraforge-core','--all-targets','--','-D','warnings')
Run-Check 'pnpm' @('build')
if ($Package) { Run-Check 'pnpm' @('package') }
