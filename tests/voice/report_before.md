# Voice regression report

2026-10-15 17:56 · dictation `gemini-3.5-transcribe-live` · commands `gemini-3.8-live` · REST `gemini-3.1-flash-lite` → `gemini-3.5-flash-lite` · language `auto` · wake word `Soul` · tools **dry-run**

**23 cases: 16 PASS · 6 FAIL · 0 ERROR · 1 SKIP** · median latency 584 ms · run took 114 s

Latency = talk key released → decision (tool chosen or dictation text ready); `stt` = transcript ready. *synthetic* = Gemini TTS sample.

| file | transcript | route | tool+args | expected | result | latency ms |
|---|---|---|---|---|---|---|
| nastepna.wav | Następna piosenka. | command (command-verb) | media_control {"action":"next"} [pre-router] | command · media_control {"action":"next"} | ✅ PASS | 227 (stt 226) |
| dyktowanie_faktura.wav |  |  | — | dictation · text∋"faktur" | ⏭️ SKIP |  |
| pauza.wav | Zapauzuj muzykę. | dictation | paste „Zapauzuj muzykę.” | command | ❌ FAIL | 515 (stt 515) |
| wznow.wav | Odtwarzaj muzykę. | dictation | paste „Odtwarzaj muzykę.” | command | ❌ FAIL | 511 (stt 511) |
| co_gra.wav | Co teraz mi leci? | dictation | paste „Co teraz mi leci?” | command | ❌ FAIL | 463 (stt 463) |
| otworz_discord.wav | Otwórz Discord. | command (command-verb) | open_app {"name":"Discord"} [live] | command | ✅ PASS | 1467 (stt 1092) |
| zamknij_spotify.wav | Zamknij Spotify. | command (command-verb) | close_app {"name":"Spotify"} [live] | command | ✅ PASS | 937 (stt 474) |
| wez_znajdz.wav | Znajdź jak ugotować pierogi. | command (command-verb) | web_search {"query":"jak ugotować pierogi"} [pre-router] | command | ✅ PASS | 503 (stt 502) |
| pytanie_spaghetti.wav | Słuchaj, jak ugotować spaghetti? | dictation | paste „Słuchaj, jak ugotować spaghetti?” | command | ❌ FAIL | 506 (stt 506) |
| zapamietaj.wav | Zapamiętaj, że gram na serwerze Realm RP. | command (command-verb) | remember {"fact":"Gram na serwerze Realm RP"} [pre-router] | command | ✅ PASS | 467 (stt 465) |
| co_pamietasz.wav | Co pamiętasz o Silverze? | command (pre-router) | recall {"query":"Silverze"} [pre-router] | command | ✅ PASS | 1737 (stt 1736) |
| timer.wav | Ustaw timer 10 minut na makaron. | command (command-verb) | set_timer {"label":"makaron","seconds":600} [pre-router] | command | ✅ PASS | 584 (stt 583) |
| anuluj_timer.wav | Anuluj minutnik. | command (pre-router) | cancel_timer {} [pre-router] | command | ✅ PASS | 964 (stt 964) |
| scena_granie.wav | Gramy w five'a. | dictation | paste „Gramy w five'a.” | command | ❌ FAIL | 616 (stt 616) |
| dzien_dobry.wav | Dzień dobry. | dictation | paste „Dzień dobry.” | command | ❌ FAIL | 590 (stt 590) |
| mow_ciszej.wav | Mów ciszej. | command (command-verb) | set_voice_volume {"delta":-20} [pre-router] | command | ✅ PASS | 496 (stt 495) |
| dykt_faktura.wav | Dzień dobry, przesyłam w załączniku fakturę za październik. | dictation | paste „Dzień dobry, przesyłam w załączniku fakturę za październik.” | dictation | ✅ PASS | 472 (stt 472) |
| dykt_poprawka.wav | Spotkanie przenosimy na piątek o 10:00. | dictation | paste „Spotkanie przenosimy na piątek o 10:00.” | dictation | ✅ PASS | 920 (stt 920) |
| dykt_kefir.wav | Kup mleko, to znaczy kefir i chleb. | dictation | paste „Kup kefir i chleb.” | dictation | ✅ PASS | 1292 (stt 475) |
| dykt_polskie.wav | Zarzuty: gęsią, jaźń. | dictation | paste „Zarzuty: gęsią, jaźń.” | dictation | ✅ PASS | 514 (stt 514) |
| dykt_angielski.wav | Let's meet tomorrow at 5:00. | dictation | paste „Let's meet tomorrow at 5:00.” | dictation | ✅ PASS | 1203 (stt 1203) |
| pulapka_otworz.wav | Odwiedziłem wczoraj nowy sklep kolegi. | dictation | paste „Odwiedziłem wczoraj nowy sklep kolegi.” | dictation | ✅ PASS | 650 (stt 650) |
| pulapka_prosze.wav | Proszę o przesłanie raportu do piątku. | dictation | paste „Proszę o przesłanie raportu do piątku.” | dictation | ✅ PASS | 473 (stt 472) |

## Details

- **dyktowanie_faktura.wav** (SKIP): WAV not recorded yet
- **pauza.wav** (FAIL): route dictation ≠ command
- **wznow.wav** (FAIL): route dictation ≠ command
- **co_gra.wav** (FAIL): route dictation ≠ command
- **pytanie_spaghetti.wav** (FAIL): route dictation ≠ command
- **scena_granie.wav** (FAIL): route dictation ≠ command
- **dzien_dobry.wav** (FAIL): route dictation ≠ command
