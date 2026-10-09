param(
    [ValidateSet('Empty', 'Configured', 'BadPre', 'BadPost')]
    [string]$Mode = 'Empty'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$executable = Join-Path $repo 'target\debug\knot.exe'
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "Build the product first: cargo build --locked --bin knot"
}

$tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\')
$fixture = Join-Path $tempRoot ("knot-d029-" + [guid]::NewGuid().ToString('N'))
$xdgHome = Join-Path $fixture 'xdg'
$configRoot = Join-Path $xdgHome 'knot'
$stdoutPath = Join-Path $fixture 'stdout.txt'
$stderrPath = Join-Path $fixture 'stderr.txt'
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Write-FixtureFile([string]$Name, [string]$Content) {
    [System.IO.File]::WriteAllText((Join-Path $configRoot $Name), $Content, $utf8)
}

try {
    New-Item -ItemType Directory -Path $configRoot -Force | Out-Null
    switch ($Mode) {
        'Configured' {
            Write-FixtureFile 'shared.mjs' 'export const state = { stage: "new" };'
            Write-FixtureFile 'pre-init.js' @'
import * as knot from 'knot';
import { state } from './shared.mjs';
state.stage = 'pre';
await knot.commands.register('d029.pre-ready', () => {});
'@
            Write-FixtureFile 'post-init.js' @'
import * as knot from 'knot';
import { state } from './shared.mjs';
if (state.stage !== 'pre') throw new Error('D029 shared state was not preserved');
await knot.commands.register('d029.post-saw-pre', () => {});
knot.keybinding('ctrl-shift-p', 'workbench.show-command-palette');
'@
        }
        'BadPre' {
            Write-FixtureFile 'pre-init.js' 'export const = ;'
        }
        'BadPost' {
            Write-FixtureFile 'pre-init.js' 'export const ready = true;'
            Write-FixtureFile 'post-init.js' "throw new Error('D029 post-init failure');"
        }
    }

    $extensionRoot = Join-Path ([System.Environment]::GetFolderPath('LocalApplicationData')) 'Knot\data\extensions'
    $extensionState = if (Test-Path -LiteralPath $extensionRoot -PathType Container) {
        'exists'
    } elseif (Test-Path -LiteralPath $extensionRoot) {
        'non-directory'
    } else {
        'missing'
    }
    $env:XDG_CONFIG_HOME = $xdgHome
    Write-Output "mode=$Mode"
    Write-Output "executable=$executable"
    Write-Output "XDG_CONFIG_HOME=$xdgHome"
    Write-Output "selected_config_root=$configRoot"
    Write-Output "installed_extensions_root=$extensionRoot ($extensionState)"
    Write-Output "stdout=$stdoutPath"
    Write-Output "stderr=$stderrPath"

    $product = Start-Process -FilePath $executable -WorkingDirectory $repo -PassThru `
        -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
    Write-Output "pid=$($product.Id)"
    $product.WaitForExit()
    Write-Output 'process_exited=true'
    if (Test-Path -LiteralPath $stderrPath) {
        Get-Content -LiteralPath $stderrPath | ForEach-Object { Write-Output "stderr: $_" }
    }
} finally {
    $resolvedFixture = [System.IO.Path]::GetFullPath($fixture)
    if ([System.IO.Path]::GetDirectoryName($resolvedFixture) -ne $tempRoot -or
        -not [System.IO.Path]::GetFileName($resolvedFixture).StartsWith('knot-d029-')) {
        throw "Refusing to remove unexpected fixture path: $resolvedFixture"
    }
    if (Test-Path -LiteralPath $resolvedFixture) {
        Remove-Item -LiteralPath $resolvedFixture -Recurse -Force
    }
}
