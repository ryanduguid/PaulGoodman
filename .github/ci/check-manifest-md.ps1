# Validate synthetic extracted rows without fetching a title or building the corpus.
$ErrorActionPreference = 'Stop'
$schema = Join-Path $PSScriptRoot '../../docs/manifest-md.schema.json'
$fixture = Join-Path $PSScriptRoot '../../tests/corpus/fixtures/manifest-md.json'
function Test-Manifest([string]$Json) {
    if (!(Test-Json -Json $Json -SchemaFile $schema -ErrorAction SilentlyContinue)) { return $false }
    # Test-Json treats the schema's date format as an annotation.
    foreach ($row in (ConvertFrom-Json -InputObject $Json -AsHashtable -DateKind String)) {
        foreach ($key in 'versionStart', 'retrieved', 'current_version_start') {
            if (!$row.Contains($key)) { continue }
            $date = [DateOnly]::MinValue
            if (![DateOnly]::TryParseExact($row[$key], 'yyyy-MM-dd',
                    [cultureinfo]::InvariantCulture, [Globalization.DateTimeStyles]::None,
                    [ref]$date)) { return $false }
        }
    }
    return $true
}
$source = Get-Content -Raw -LiteralPath $fixture
if (!(Test-Manifest $source)) { throw 'Invalid positive fixture' }
foreach ($edit in @(
    { param($rows) $rows[0].id = '../id' },
    { param($rows) $rows[0].Remove('sourceUrl') },
    { param($rows) $rows[0].words = -1 },
    { param($rows) $rows[1].version_is_current = $true },
    { param($rows) $rows[0].retrieved = '2026-02-29' }
)) {
    $rows = ConvertFrom-Json -InputObject $source -AsHashtable -DateKind String -NoEnumerate
    & $edit $rows
    $invalid = ConvertTo-Json -Depth 5 -InputObject $rows
    if (Test-Manifest $invalid) {
        throw 'Invalid manifest row passed validation'
    }
}
Write-Output 'Validated cached and superseded rows and rejected 5 invalid variants.'
