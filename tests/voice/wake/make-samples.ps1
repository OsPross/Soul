# Generates the synthetic wake word set (offline, Windows TTS — no API key):
#   train/          8 x "Soul" by one voice (OneCore Adam), varied rate / pitch  -> the model
#   positive/       the same voice, held-out rates / pitch and "Soul" inside a phrase
#   positive_other/ other voices saying "Soul" (speaker-independence, reported separately)
#   negative/       unrelated words and phrases (PL + EN), several voices
#   negative_hard/  similar-sounding single words (sól, stół, solo, sold, soup…)
# Real recordings beat TTS: copy your Settings training samples (app data dir, wakeword/samples)
# into train/ and your own "Soul" takes into positive/ to measure your voice.
#
#   powershell -ExecutionPolicy Bypass -File tests/voice/wake/make-samples.ps1
param([switch]$Force)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

Add-Type -AssemblyName System.Speech
Add-Type -AssemblyName System.Runtime.WindowsRuntime
[void][Windows.Media.SpeechSynthesis.SpeechSynthesizer, Windows.Media.SpeechSynthesis, ContentType = WindowsRuntime]

$asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq "AsTask" -and $_.GetParameters().Count -eq 1 -and
    $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
} | Select-Object -First 1

function Out-Path($dir, $name) {
    $d = Join-Path $root $dir
    New-Item -ItemType Directory -Force $d | Out-Null
    return Join-Path $d $name
}

# OneCore (WinRT) voice: rate 0.5–3, pitch 0–2.
function Say-OneCore($voiceLike, $text, $rate, $pitch, $path) {
    if ((Test-Path $path) -and -not $Force) { return }
    $synth = New-Object Windows.Media.SpeechSynthesis.SpeechSynthesizer
    $voice = [Windows.Media.SpeechSynthesis.SpeechSynthesizer]::AllVoices | Where-Object { $_.DisplayName -like "*$voiceLike*" } | Select-Object -First 1
    if (-not $voice) { Write-Warning "OneCore voice '$voiceLike' not installed"; return }
    $synth.Voice = $voice
    $synth.Options.SpeakingRate = $rate
    $synth.Options.AudioPitch = $pitch
    $task = $asTask.MakeGenericMethod([Windows.Media.SpeechSynthesis.SpeechSynthesisStream]).Invoke($null, @($synth.SynthesizeTextToStreamAsync($text)))
    $task.Wait()
    $src = [System.IO.WindowsRuntimeStreamExtensions]::AsStreamForRead($task.Result)
    $dst = [System.IO.File]::Create($path)
    $src.CopyTo($dst)
    $dst.Close(); $src.Close(); $synth.Dispose()
}

# Desktop SAPI voice: rate -10..10, 16 kHz mono.
function Say-Sapi($voiceLike, $text, $rate, $path) {
    if ((Test-Path $path) -and -not $Force) { return }
    $s = New-Object System.Speech.Synthesis.SpeechSynthesizer
    $v = $s.GetInstalledVoices() | Where-Object { $_.VoiceInfo.Name -like "*$voiceLike*" } | Select-Object -First 1
    if (-not $v) { Write-Warning "SAPI voice '$voiceLike' not installed"; return }
    $s.SelectVoice($v.VoiceInfo.Name)
    $s.Rate = $rate
    $fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)
    $s.SetOutputToWaveFile($path, $fmt)
    $s.Speak($text)
    $s.Dispose()
}

# --- train: one speaker, 8 takes ---
$train = @(@(1.0, 1.0), @(0.9, 1.0), @(1.1, 1.0), @(1.0, 0.9), @(1.0, 1.1), @(0.85, 0.95), @(1.15, 1.05), @(1.05, 0.95))
$i = 1
foreach ($t in $train) { Say-OneCore "Adam" "Soul" $t[0] $t[1] (Out-Path "train" ("adam_{0}.wav" -f $i)); $i++ }

# --- positives: same speaker, held-out takes and in-phrase ---
$pos = @(
    @("Soul", 0.95, 1.05, "solo_a"), @("Soul", 1.25, 1.0, "solo_fast"), @("Soul", 0.8, 1.0, "solo_slow"),
    @("Soul", 1.0, 1.2, "solo_high"), @("Soul", 1.0, 0.8, "solo_low"),
    @("Hej Soul", 1.0, 1.0, "hej_soul"), @("Soul, otwórz Discord", 1.0, 1.0, "soul_otworz"),
    @("Soul, jaka jest pogoda?", 1.05, 1.0, "soul_pogoda"), @("No dobra. Soul, pauza.", 1.0, 1.0, "soul_pauza"),
    @("Soul, następna piosenka", 1.1, 1.0, "soul_nastepna")
)
foreach ($p in $pos) { Say-OneCore "Adam" $p[0] $p[1] $p[2] (Out-Path "positive" ("adam_{0}.wav" -f $p[3])) }

# --- other speakers saying "Soul" ---
Say-OneCore "Paulina" "Soul" 1.0 1.0 (Out-Path "positive_other" "paulina1_soul.wav")
Say-OneCore "Paulina" "Hej Soul" 1.0 1.0 (Out-Path "positive_other" "paulina1_hej_soul.wav")
Say-Sapi "Paulina" "Soul" 0 (Out-Path "positive_other" "paulina2_soul.wav")
Say-Sapi "Zira" "Soul" 0 (Out-Path "positive_other" "zira_soul.wav")
Say-Sapi "Zira" "Hey Soul, open Spotify" 0 (Out-Path "positive_other" "zira_hey_soul.wav")

# --- hard negatives: similar-sounding single words ---
$hardPl = @(@("sól", "sol"), @("sowa", "sowa"), @("solo", "solo"), @("Paweł", "pawel"), @("dół", "dol"), @("stół", "stol"))
foreach ($n in $hardPl) {
    Say-OneCore "Adam" $n[0] 1.0 1.0 (Out-Path "negative_hard" ("adam_{0}.wav" -f $n[1]))
    Say-OneCore "Paulina" $n[0] 1.0 1.0 (Out-Path "negative_hard" ("paulina_{0}.wav" -f $n[1]))
}
$hardEn = @(@("soup", "soup"), @("sold", "sold"), @("cool", "cool"), @("school", "school"), @("sound", "sound"), @("so long", "so_long"))
foreach ($n in $hardEn) { Say-Sapi "Zira" $n[0] 0 (Out-Path "negative_hard" ("zira_{0}.wav" -f $n[1])) }

# --- negatives: unrelated words and phrases, PL + EN, several voices ---
$negPl = @(
    @("dzień dobry", "dzien_dobry"), @("otwórz Discord", "otworz_discord"), @("następna piosenka", "nastepna"),
    @("zrób mi kawę", "kawa"), @("jaka jest dzisiaj pogoda", "pogoda"), @("pauza", "pauza"), @("szukaj przepisu na spaghetti", "spaghetti"),
    @("Wczoraj wieczorem poszliśmy z Olą do kina, a potem długo rozmawialiśmy o pracy, wakacjach i o tym, co ugotować w niedzielę.", "dlugie_zdanie")
)
foreach ($n in $negPl) {
    Say-OneCore "Adam" $n[0] 1.0 1.0 (Out-Path "negative" ("adam_{0}.wav" -f $n[1]))
    Say-OneCore "Paulina" $n[0] 1.0 1.0 (Out-Path "negative" ("paulina_{0}.wav" -f $n[1]))
}
$negEn = @(
    @("good morning", "good_morning"), @("what is the weather like today", "weather"),
    @("Yesterday we walked along the river, talked about the new project and planned a short trip for the weekend.", "long_sentence")
)
foreach ($n in $negEn) { Say-Sapi "Zira" $n[0] 0 (Out-Path "negative" ("zira_{0}.wav" -f $n[1])) }

Write-Host "Done. train/, positive/, positive_other/, negative/, negative_hard/ under $root"
