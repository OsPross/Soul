# Wake word report

rustpotter 3.0.2 · model from 8 takes (`train/`, built in 94 ms) · 2026-10-15 18:59 · offline, no API

| sensitivity | threshold / hits | same speaker (clean) | +noise 15 dB | −12 dB | other speakers | false positives | negative audio | FP / hour | similar words fired | CPU (1 core) |
|---|---|---|---|---|---|---|---|---|---|---|
| 0.30 | 0.652 / 10 | 9/10 (90%) | 6/10 (60%) | 9/10 (90%) | 0/5 (0%) | 0 | 2.6 min | 0.0 | 0/18 (0%) | 3.80% |
| 0.50 | 0.640 / 8 | 10/10 (100%) | 8/10 (80%) | 10/10 (100%) | 1/5 (20%) | 0 | 2.6 min | 0.0 | 1/18 (6%) | 3.89% |
| 0.70 | 0.628 / 6 | 10/10 (100%) | 9/10 (90%) | 10/10 (100%) | 3/5 (60%) | 2 | 2.6 min | 46.1 | 4/18 (22%) | 3.94% |

False positives at 0.70: ../dzien_dobry.wav (1×), adam_dlugie_zdanie.wav (1×)

CPU = detector time / audio time on one core of this machine (24 logical cores; divide by the core count for whole-machine %). Plain `cargo test` builds are unoptimized; see tests/voice/README.md for an optimized run.

## Per file (sensitivity 0.50, threshold 0.64)

| set | file | sec | fired | score | best probe score |
|---|---|---|---|---|---|
| neg_hard | adam_dol.wav | 1.1 | 0 | – | 0.575 |
| neg_hard | adam_pawel.wav | 1.3 | 0 | – | 0.560 |
| neg_hard | adam_sol.wav | 1.2 | 1 (similar word) | 0.685 | 0.685 |
| neg_hard | adam_solo.wav | 1.2 | 0 | – | 0.639 |
| neg_hard | adam_sowa.wav | 1.3 | 0 | – | 0.645 |
| neg_hard | adam_stol.wav | 1.2 | 0 | – | 0.653 |
| neg_hard | paulina_dol.wav | 1.2 | 0 | – | 0.587 |
| neg_hard | paulina_pawel.wav | 1.3 | 0 | – | 0.592 |
| neg_hard | paulina_sol.wav | 1.2 | 0 | – | 0.641 |
| neg_hard | paulina_solo.wav | 1.3 | 0 | – | 0.584 |
| neg_hard | paulina_sowa.wav | 1.3 | 0 | – | 0.550 |
| neg_hard | paulina_stol.wav | 1.3 | 0 | – | 0.656 |
| neg_hard | zira_cool.wav | 1.3 | 0 | – | 0.559 |
| neg_hard | zira_school.wav | 1.4 | 0 | – | 0.590 |
| neg_hard | zira_so_long.wav | 1.5 | 0 | – | 0.605 |
| neg_hard | zira_sold.wav | 1.4 | 0 | – | 0.649 |
| neg_hard | zira_sound.wav | 1.4 | 0 | – | 0.591 |
| neg_hard | zira_soup.wav | 1.4 | 0 | – | 0.591 |
| neg_noise | white noise 60 s (-30 dBFS) | 60.0 | 0 | – | 0.362 |
| neg_real | ../anuluj_timer.wav | 1.0 | 0 | – | 0.616 |
| neg_real | ../co_gra.wav | 2.4 | 0 | – | 0.537 |
| neg_real | ../co_pamietasz.wav | 3.2 | 0 | – | 0.610 |
| neg_real | ../dykt_angielski.wav | 1.5 | 0 | – | 0.582 |
| neg_real | ../dykt_faktura.wav | 2.7 | 0 | – | 0.585 |
| neg_real | ../dykt_kefir.wav | 3.1 | 0 | – | 0.619 |
| neg_real | ../dykt_polskie.wav | 2.6 | 0 | – | 0.602 |
| neg_real | ../dykt_poprawka.wav | 4.5 | 0 | – | 0.605 |
| neg_real | ../dzien_dobry.wav | 1.6 | 0 | – | 0.636 |
| neg_real | ../mow_ciszej.wav | 0.8 | 0 | – | 0.597 |
| neg_real | ../nastepna.wav | 1.1 | 0 | – | 0.551 |
| neg_real | ../otworz_discord.wav | 0.9 | 0 | – | 0.467 |
| neg_real | ../pauza.wav | 1.3 | 0 | – | 0.486 |
| neg_real | ../pulapka_otworz.wav | 1.7 | 0 | – | 0.589 |
| neg_real | ../pulapka_prosze.wav | 1.9 | 0 | – | 0.617 |
| neg_real | ../pytanie_spaghetti.wav | 1.8 | 0 | – | 0.642 |
| neg_real | ../scena_granie.wav | 1.3 | 0 | – | 0.629 |
| neg_real | ../timer.wav | 2.1 | 0 | – | 0.585 |
| neg_real | ../wez_znajdz.wav | 1.6 | 0 | – | 0.580 |
| neg_real | ../wznow.wav | 1.0 | 0 | – | 0.556 |
| neg_real | ../zamknij_spotify.wav | 1.1 | 0 | – | 0.502 |
| neg_real | ../zapamietaj.wav | 2.9 | 0 | – | 0.538 |
| neg_tts | adam_dlugie_zdanie.wav | 8.5 | 0 | – | 0.655 |
| neg_tts | adam_dzien_dobry.wav | 1.5 | 0 | – | 0.542 |
| neg_tts | adam_kawa.wav | 1.7 | 0 | – | 0.580 |
| neg_tts | adam_nastepna.wav | 2.1 | 0 | – | 0.565 |
| neg_tts | adam_otworz_discord.wav | 1.9 | 0 | – | 0.607 |
| neg_tts | adam_pauza.wav | 1.3 | 0 | – | 0.552 |
| neg_tts | adam_pogoda.wav | 2.4 | 0 | – | 0.542 |
| neg_tts | adam_spaghetti.wav | 2.7 | 0 | – | 0.575 |
| neg_tts | paulina_dlugie_zdanie.wav | 8.3 | 0 | – | 0.622 |
| neg_tts | paulina_dzien_dobry.wav | 1.6 | 0 | – | 0.545 |
| neg_tts | paulina_kawa.wav | 1.7 | 0 | – | 0.567 |
| neg_tts | paulina_nastepna.wav | 2.1 | 0 | – | 0.594 |
| neg_tts | paulina_otworz_discord.wav | 1.8 | 0 | – | 0.584 |
| neg_tts | paulina_pauza.wav | 1.4 | 0 | – | 0.572 |
| neg_tts | paulina_pogoda.wav | 2.3 | 0 | – | 0.593 |
| neg_tts | paulina_spaghetti.wav | 2.5 | 0 | – | 0.566 |
| neg_tts | zira_good_morning.wav | 1.5 | 0 | – | 0.570 |
| neg_tts | zira_long_sentence.wav | 6.4 | 0 | – | 0.578 |
| neg_tts | zira_weather.wav | 2.2 | 0 | – | 0.565 |
| pos_clean | adam_hej_soul.wav | 1.5 | 1 | 0.688 | 0.688 |
| pos_clean | adam_solo_a.wav | 1.3 | 1 | 0.712 | 0.712 |
| pos_clean | adam_solo_fast.wav | 1.0 | 1 | 0.694 | 0.694 |
| pos_clean | adam_solo_high.wav | 1.3 | 1 | 0.712 | 0.712 |
| pos_clean | adam_solo_low.wav | 1.3 | 1 | 0.710 | 0.710 |
| pos_clean | adam_solo_slow.wav | 1.6 | 1 | 0.710 | 0.710 |
| pos_clean | adam_soul_nastepna.wav | 2.5 | 1 | 0.706 | 0.706 |
| pos_clean | adam_soul_otworz.wav | 2.4 | 1 | 0.710 | 0.710 |
| pos_clean | adam_soul_pauza.wav | 3.4 | 1 | 0.713 | 0.713 |
| pos_clean | adam_soul_pogoda.wav | 2.4 | 1 | 0.708 | 0.708 |
| pos_noisy | adam_hej_soul.wav +noise15dB | 1.5 | 0 ✗ miss | – | 0.615 |
| pos_noisy | adam_solo_a.wav +noise15dB | 1.3 | 1 | 0.683 | 0.683 |
| pos_noisy | adam_solo_fast.wav +noise15dB | 1.0 | 0 ✗ miss | – | 0.676 |
| pos_noisy | adam_solo_high.wav +noise15dB | 1.3 | 1 | 0.683 | 0.683 |
| pos_noisy | adam_solo_low.wav +noise15dB | 1.3 | 1 | 0.682 | 0.682 |
| pos_noisy | adam_solo_slow.wav +noise15dB | 1.6 | 1 | 0.686 | 0.686 |
| pos_noisy | adam_soul_nastepna.wav +noise15dB | 2.5 | 1 | 0.680 | 0.680 |
| pos_noisy | adam_soul_otworz.wav +noise15dB | 2.4 | 1 | 0.687 | 0.687 |
| pos_noisy | adam_soul_pauza.wav +noise15dB | 3.4 | 1 | 0.693 | 0.693 |
| pos_noisy | adam_soul_pogoda.wav +noise15dB | 2.4 | 1 | 0.677 | 0.677 |
| pos_other | paulina1_hej_soul.wav | 1.5 | 0 ✗ miss | – | 0.604 |
| pos_other | paulina1_soul.wav | 1.3 | 0 ✗ miss | – | 0.650 |
| pos_other | paulina2_soul.wav | 1.3 | 0 ✗ miss | – | 0.650 |
| pos_other | zira_hey_soul.wav | 2.8 | 0 ✗ miss | – | 0.633 |
| pos_other | zira_soul.wav | 1.4 | 1 | 0.647 | 0.647 |
| pos_quiet | adam_hej_soul.wav -12dB | 1.5 | 1 | 0.688 | 0.688 |
| pos_quiet | adam_solo_a.wav -12dB | 1.3 | 1 | 0.712 | 0.712 |
| pos_quiet | adam_solo_fast.wav -12dB | 1.0 | 1 | 0.694 | 0.694 |
| pos_quiet | adam_solo_high.wav -12dB | 1.3 | 1 | 0.712 | 0.712 |
| pos_quiet | adam_solo_low.wav -12dB | 1.3 | 1 | 0.710 | 0.710 |
| pos_quiet | adam_solo_slow.wav -12dB | 1.6 | 1 | 0.710 | 0.710 |
| pos_quiet | adam_soul_nastepna.wav -12dB | 2.5 | 1 | 0.706 | 0.706 |
| pos_quiet | adam_soul_otworz.wav -12dB | 2.4 | 1 | 0.710 | 0.710 |
| pos_quiet | adam_soul_pauza.wav -12dB | 3.4 | 1 | 0.713 | 0.713 |
| pos_quiet | adam_soul_pogoda.wav -12dB | 2.4 | 1 | 0.708 | 0.708 |
