# Voice regression report

2026-10-15 18:09 · dictation `gemini-3.5-transcribe-live` · commands `gemini-3.8-live` · REST `gemini-3.1-flash-lite` → `gemini-3.5-flash-lite` · language `auto` · wake word `Soul` · tools **dry-run**

**22 cases: 21 PASS · 1 FAIL · 0 ERROR · 0 SKIP** · median latency 485 ms · run took 111 s

Latency = talk key released → decision (tool chosen or dictation text ready); `stt` = transcript ready. *synthetic* = Gemini TTS sample.

| file | transcript | route | tool+args | expected | result | latency ms |
|---|---|---|---|---|---|---|
| nastepna.wav | Następna piosenka. | command (command-verb) | media_control {"action":"next"} [pre-router] | command · media_control {"action":"next"} | ✅ PASS | 261 (stt 259) |
| pauza.wav | Zapauzuj muzykę. | command (command-verb) | media_control {"action":"pause"} [pre-router] | command · media_control {"action":"pause"} | ✅ PASS | 425 (stt 424) |
| wznow.wav | Odtwarzaj muzykę. | command (command-verb) | media_control {"action":"play"} [pre-router] | command · media_control {"action":"play"} | ✅ PASS | 436 (stt 435) |
| co_gra.wav | Co teraz mi leci? | command (pre-router) | now_playing {} [pre-router] | command · now_playing | ✅ PASS | 497 (stt 496) |
| otworz_discord.wav | Otwórz Discord. | command (command-verb) | open_app {"name":"Discord"} [live] | command · open_app {"name":"discord"} | ✅ PASS | 877 (stt 556) |
| zamknij_spotify.wav | Zamknij Spotify. | command (command-verb) | close_app {"name":"Spotify"} [live] | command · close_app {"name":"spotify"} | ✅ PASS | 759 (stt 447) |
| wez_znajdz.wav | Znajdź jak ugotować pierogi. | command (command-verb) | web_search {"query":"jak ugotować pierogi"} [pre-router] | command · web_search {"query":"pierogi"} | ✅ PASS | 617 (stt 615) |
| pytanie_spaghetti.wav | Słuchaj, jak ugotować spaghetti? | command (question) | answer {"text":"Gotuj makaron w dużej ilości osolonej wody przez czas podany na opakowani… | command | ✅ PASS | 2641 (stt 486) |
| zapamietaj.wav | Zapamiętaj, że gram na serwerze Realm RP. | command (command-verb) | remember {"fact":"Gram na serwerze Realm RP"} [pre-router] | command · remember | ✅ PASS | 485 (stt 484) |
| co_pamietasz.wav | Co pamiętasz o Silverze? | command (pre-router) | recall {"query":"Silverze"} [pre-router] | command · recall | ✅ PASS | 449 (stt 448) |
| timer.wav | Ustaw timer 10 minut na makaron. | command (command-verb) | set_timer {"label":"makaron","seconds":600} [pre-router] | command · set_timer {"seconds":600} | ✅ PASS | 484 (stt 482) |
| anuluj_timer.wav | Anuluj minutnik. | command (pre-router) | cancel_timer {} [pre-router] | command · cancel_timer | ✅ PASS | 479 (stt 478) |
| scena_granie.wav | Gramy w five'a. | command (scene-trigger) | run_scene {"name":"granie"} [pre-router] | command · run_scene {"name":"granie"} | ✅ PASS | 518 (stt 517) |
| dzien_dobry.wav | Dzień dobry. | command (greeting) | morning_briefing {} [pre-router] | command · morning_briefing | ✅ PASS | 423 (stt 422) |
| mow_ciszej.wav | Mów ciszej. | command (command-verb) | set_voice_volume {"delta":-20} [pre-router] | command · set_voice_volume | ✅ PASS | 461 (stt 460) |
| dykt_faktura.wav | Dzień dobry, przesyłam w załączniku fakturę za październik. | dictation | paste „Dzień dobry, przesyłam w załączniku fakturę za październik.” | dictation · text∋["faktur","październik"] | ✅ PASS | 482 (stt 482) |
| dykt_poprawka.wav | Spotkanie przenosimy na piątek o 10:00. | dictation | paste „Spotkanie przenosimy na piątek o 10:00.” | dictation · text∋"piątek" | ✅ PASS | 473 (stt 473) |
| dykt_kefir.wav | Kup mleko, to znaczy kefir i chleb. | dictation | paste „Kup kefir i chleb.” | dictation · text∋["kefir","chleb"] | ✅ PASS | 1100 (stt 473) |
| dykt_polskie.wav | Zarzuty: gęsią, jaźń. | dictation | paste „Zarzuty: gęsią, jaźń.” | dictation · text=„Zażółć gęślą jaźń” | ❌ FAIL | 511 (stt 511) |
| dykt_angielski.wav | Let's meet tomorrow at 5:00. | dictation | paste „Let's meet tomorrow at 5:00.” | dictation · text∋["meet","tomorrow"] | ✅ PASS | 461 (stt 461) |
| pulapka_otworz.wav | Odwiedziłem wczoraj nowy sklep kolegi. | dictation | paste „Odwiedziłem wczoraj nowy sklep kolegi.” | dictation | ✅ PASS | 508 (stt 508) |
| pulapka_prosze.wav | Proszę o przesłanie raportu do piątku. | dictation | paste „Proszę o przesłanie raportu do piątku.” | dictation | ✅ PASS | 521 (stt 521) |

## Details

- **dykt_polskie.wav** (FAIL): text „Zarzuty: gęsią, jaźń.” ≠ „Zażółć gęślą jaźń”
