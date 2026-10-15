# Dictation A/B: the same dykt_* samples with three transcription setups, one report each.
#   $env:SOUL_TEST_KEY = "<key>"; .\tests\voice\ab-dictation.ps1
#   A  language auto  (pl-PL + en-US hints, the app default)  -> report_ab_auto.md
#   B  language pl    (pl-PL only)                            -> report_ab_pl.md
#   C  language auto  + Polish transcription prompt           -> report_ab_prompt.md
param([string]$Filter = "dykt")

$env:SOUL_VOICE_FILTER = $Filter
$runs = @(
    @{ Name = "auto";   Language = "auto"; Prompt = "0" },
    @{ Name = "pl";     Language = "pl";   Prompt = "0" },
    @{ Name = "prompt"; Language = "auto"; Prompt = "1" }
)
foreach ($run in $runs) {
    Write-Host "=== dictation A/B: $($run.Name) ===" -ForegroundColor Cyan
    $env:SOUL_TEST_LANGUAGE = $run.Language
    $env:SOUL_TEST_DICTATION_PROMPT = $run.Prompt
    $env:SOUL_VOICE_REPORT = "report_ab_$($run.Name).md"
    npm run test:voice
}
Remove-Item Env:SOUL_VOICE_FILTER, Env:SOUL_TEST_LANGUAGE, Env:SOUL_TEST_DICTATION_PROMPT, Env:SOUL_VOICE_REPORT -ErrorAction SilentlyContinue
Write-Host "Reports: tests/voice/report_ab_auto.md, report_ab_pl.md, report_ab_prompt.md"
