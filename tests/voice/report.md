# Voice regression report

2026-10-15 23:35 · dictation `gemini-3.5-transcribe-live` · commands `gemini-3.8-live` · REST `gemini-3.1-flash-lite` → `gemini-3.5-flash-lite` · language `auto` · wake word `Soul` · tools **dry-run**

**28 cases: 25 PASS · 2 FAIL · 0 ERROR · 1 SKIP** · median latency 505 ms · run took 137 s

Latency = talk key released → decision (tool chosen or dictation text ready); `stt` = transcript ready. *synthetic* = Gemini TTS sample.

| file | transcript | route | tool+args | expected | result | latency ms |
|---|---|---|---|---|---|---|
| nastepna.wav | Następna piosenka. | command (command-verb) | media_control {"action":"next"} [pre-router] | command · media_control {"action":"next"} | ✅ PASS | 250 (stt 248) |
| pauza.wav | Zapauzuj muzykę. | command (command-verb) | media_control {"action":"pause"} [pre-router] | command · media_control {"action":"pause"} | ✅ PASS | 522 (stt 520) |
| wznow.wav | Odtwarzaj muzykę. | command (command-verb) | media_control {"action":"play"} [pre-router] | command · media_control {"action":"play"} | ✅ PASS | 482 (stt 480) |
| co_gra.wav | Co teraz mi leci? | command (pre-router) | now_playing {} [pre-router] | command · now_playing | ✅ PASS | 462 (stt 461) |
| otworz_discord.wav | Otwórz Discord. | command (command-verb) | open_app {"name":"Discord"} [live] | command · open_app {"name":"discord"} | ✅ PASS | 785 (stt 457) |
| zamknij_spotify.wav | Zamknij Spotify. | command (command-verb) | close_app {"name":"Spotify"} [live] | command · close_app {"name":"spotify"} | ✅ PASS | 749 (stt 447) |
| wez_znajdz.wav | Znajdź jak ugotować pierogi. | command (command-verb) | web_search {"query":"jak ugotować pierogi"} [pre-router] | command · web_search {"query":"pierogi"} | ✅ PASS | 550 (stt 548) |
| pytanie_spaghetti.wav | Słuchaj, jak ugotować spaghetti? | command (question) | answer {"text":"Gotuj makaron w dużej ilości osolonej wody przez czas podany na opakowani… | command | ✅ PASS | 2234 (stt 513) |
| zapamietaj.wav | Zapamiętaj, że gram na serwerze Realm RP. | command (command-verb) | remember {"fact":"Gram na serwerze Realm RP"} [pre-router] | command · remember | ✅ PASS | 473 (stt 470) |
| co_pamietasz.wav | Co pamiętasz o Silverze? | command (pre-router) | recall {"query":"Silverze"} [pre-router] | command · recall | ✅ PASS | 478 (stt 476) |
| timer.wav | Ustaw timer 10 minut na makaron. | command (command-verb) | set_timer {"label":"makaron","seconds":600} [pre-router] | command · set_timer {"seconds":600} | ✅ PASS | 509 (stt 507) |
| anuluj_timer.wav | Anuluj minutnik. | command (pre-router) | cancel_timer {} [pre-router] | command · cancel_timer | ✅ PASS | 480 (stt 479) |
| scena_granie.wav | Gramy w five'a. | command (scene-trigger) | run_scene {"name":"granie"} [pre-router] | command · run_scene {"name":"granie"} | ✅ PASS | 530 (stt 528) |
| dzien_dobry.wav | Dzień dobry. | command (greeting) | morning_briefing {} [pre-router] | command · morning_briefing | ✅ PASS | 494 (stt 493) |
| mow_ciszej.wav | Mów ciszej. | command (command-verb) | set_voice_volume {"delta":-20} [pre-router] | command · set_voice_volume | ✅ PASS | 500 (stt 499) |
| dykt_faktura.wav | Dzień dobry, przesyłam w załączniku fakturę za październik. | dictation | paste „Dzień dobry, przesyłam w załączniku fakturę za październik.” | dictation · text∋["faktur","październik"] | ✅ PASS | 512 (stt 511) |
| dykt_poprawka.wav | Spotkanie przenosimy na piątek o 10:00. | dictation | paste „Spotkanie przenosimy na piątek o 10:00.” | dictation · text∋"piątek" | ✅ PASS | 505 (stt 505) |
| dykt_kefir.wav | Kup mleko, to znaczy kefir i chleb. | dictation | paste „Kup kefir i chleb.” | dictation · text∋["kefir","chleb"] | ✅ PASS | 1271 (stt 494) |
| dykt_polskie.wav | Zarzuty: gęsią, jaźń. | dictation | paste „Zarzuty: gęsią, jaźń.” | dictation · text=„Zażółć gęślą jaźń” | ❌ FAIL | 621 (stt 621) |
| dykt_angielski.wav | Let's meet tomorrow at 5:00. | dictation | paste „Let's meet tomorrow at 5:00.” | dictation · text∋["meet","tomorrow"] | ✅ PASS | 470 (stt 470) |
| pulapka_otworz.wav | Odwiedziłem wczoraj nowy sklep kolegi. | dictation | paste „Odwiedziłem wczoraj nowy sklep kolegi.” | dictation | ✅ PASS | 539 (stt 539) |
| pulapka_prosze.wav | Proszę o przesłanie raportu do piątku. | dictation | paste „Proszę o przesłanie raportu do piątku.” | dictation | ✅ PASS | 631 (stt 631) |
| followup_glosniej.wav | A ja zgłośnię. | command (followup) | set_voice_volume {"delta":20} [live] | command · set_voice_volume {"delta":20} | ✅ PASS | 986 (stt 487) |
| chain_spotify_discord.wav | Zamknij Spotify i otwórz Discord. | command (command-verb) | close_app {"name":"Spotify"} → open_app {"name":"Discord"} [pre-router] | command · close_app → open_app | ✅ PASS | 435 (stt 432) |
| chain_pauza_sciszyc.wav |  |  | — | command · media_control → set_volume | ⏭️ SKIP |  |
| tekst_przetlumacz.wav | Przetłumacz mi to na angielski. | command (text-action) | text_action {"action":"translate","language":"English"} [pre-router] | command · text_action {"action":"translate","language":"English"} | ✅ PASS | 505 (stt 503) |
| tekst_popraw.wav | Poprawa. | dictation | paste „Poprawa.” | command · text_action {"action":"fix"} | ❌ FAIL | 466 (stt 466) |
| hain_pauza_sciszyc.wav | Zapauzuj i ścisz o 20. | command (command-verb) | media_control {"action":"pause"} → set_volume {"delta":-20} [pre-router] | command | ✅ PASS | 467 (stt 464) |

## Details

- **dykt_polskie.wav** (FAIL): text „Zarzuty: gęsią, jaźń.” ≠ „Zażółć gęślą jaźń”
- **chain_pauza_sciszyc.wav** (SKIP): WAV not recorded yet
- **tekst_popraw.wav** (FAIL): route dictation ≠ command; tool — ≠ text_action; args.action — ≠ "fix"
