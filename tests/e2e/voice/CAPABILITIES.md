# Soul capabilities — voice E2E coverage map

Every capability reachable from the Smart talk key, where it is decided, and the `manifest.json` cases that
cover it. **Mode**: `real` = executed for real and verified on the result; `dry` = executed in dry-run
(`SOUL_DRY_RUN_TOOLS`), only the intended call is checked (risky / system-changing); `n/a` = not reachable by voice.

## Pipeline stages (all exercised by every case)

| Stage | Code | Checked by |
|---|---|---|
| Capture (dev WAV played into `AudioRecorder` like a mic, 20 ms blocks, real time) | `audio.rs` `play_dev_input`, `talk::begin_capture` (gated, armed after 200 ms) | `start`/`heard` trace |
| Live dictation transcription (`dictation_model`) | `ai/live.rs` `collect_dictation` | transcript similarity |
| REST fallback (silence-trimmed WAV → `ask_model` → `fallback_model`) | `lib.rs::transcribe_turn`, `audio::encode_wav_16k_mono` (`trim_silence`) | `stt=rest` in report |
| Smart routing (scene trigger → wake word / verb / pre-router / question → text action → UI command → context reset → follow-up) | `agent::classify_smart`, `agent/intent.rs` | `route` |
| Non-Polish guard | `i18n::clearly_not_polish` | `foreign_rejected` |
| Computer-task intent (before local dispatch) | `agent/computer/intent.rs::is_computer_task` | `via=computer-intent` |
| Local dispatch: context reset → text action → follow-up → chain → pre-router | `agent::local_dispatch`, `router.rs`, `chain.rs`, `context.rs` | `via=local`, tool list |
| Live commands session (text turn + tool calls, spoken reply) | `agent::run_agent_live` (`commands_model`) | `via=live`, tool list |
| REST tools fallback | `agent::run_agent_rest` | `via=rest` |
| Confirmation cards | `agent/confirm.rs` (E2E: answered from the case `confirm`) | `confirm` trace |
| Island states | `emit_overlay` (`overlay` trace: thinking / speaking / done / error / confirm) | report column |
| Conversation context (3 exchanges, 90 s) | `agent/context.rs` | follow-up cases (`fresh_context: false`) |

## Tools (Live / REST function calling + pre-router)

| Tool | Pre-router phrase | Mode | Cases |
|---|---|---|---|
| `open_app` | „otwórz Spotify” | dry | `open_spotify`, `filler_open_discord`, `chain_close_open` |
| `close_app` (+ confirm card) | „zamknij Discord” | dry | `close_discord`, `followup_close_it`, `chain_close_open` |
| `focus_app` | „przełącz na Discord” | dry | `focus_discord` |
| `open_url` → Vio tab (`vio::browse`) | „otwórz wikipedia.org” | real | `open_url_wiki` |
| `web_search` → Vio tab (+ `search_answer` for questions) | „wyszukaj …”, „jak …?” | real | `web_search_bigos`, `question_search`, `followup_same_youtube` |
| `youtube_search` → Vio tab | „wyszukaj na YouTube …” | real | `youtube_search_lofi`, `followup_same_youtube` |
| `youtube_play` → Vio tab (first result) | „puść na YouTube …” | real | `youtube_play_queen` |
| `set_volume` (`percent` / `delta`) | „ścisz o 20”, „głośność 30” | dry | `volume_down_20`, `volume_30`, `followup_again` |
| `media_control` (play/pause/next/previous) | „pauza”, „następna” | dry | `media_next`, `media_pause`, `short_dalej` |
| `type_text` | — (model only) | dry | (Live may pick it for „napisz …”; not targeted) |
| `answer` (spoken reply, no action) | — | real | `greeting`, `question_search` |
| `computer_task {goal}` → Soul Agent | UI verbs | real | see *Soul Agent / Vio* |
| `set_voice_volume` | „mów ciszej” | real (settings restored after the run) | `voice_quieter` |
| `remember` / `recall` / `forget` | „zapamiętaj …”, „co pamiętasz o …”, „zapomnij …” | real (DB restored after the run) | `memory_remember`, `memory_recall`, `memory_forget` |
| `add_note` | „zanotuj …” | real (DB restored after the run) | `note_add` |
| `set_timer` / `cancel_timer` | „minutnik na 10 minut”, „anuluj minutnik” | real | `timer_set`, `timer_cancel` |
| `create_scene` / `run_scene` (+ trigger phrase) | „uruchom scenę …” / trigger | real (scene of `open_url` + `web_search` steps only) | `scene_create`, `scene_trigger` |
| `now_playing` | „co teraz gra” | real (reads SMTC) | `now_playing` |
| `morning_briefing` | „dzień dobry” | real | `briefing` |
| `text_action` (fix / translate / summarize / formal / casual / expand / explain) | „popraw to”, „przetłumacz to na angielski” | real (selection in a Vio textarea) | `text_fix`, `text_translate` |
| `context_reset` | „nowy temat” | real | `context_reset` |
| Chained commands (≤ 4, stop at first failure) | „X i Y”, „X, potem Y” | per tool | `chain_close_open`, `chain_search_note` |

## Soul Agent / Vio (`agent/computer/`, `vio.rs`)

| Vio-backed tool | What it does | Mode | Cases |
|---|---|---|---|
| `browser_list_tabs` | `listTabs` | real | `switch_tab_article` |
| `browser_open {url \| tab_id}` | `openTab` / `activateTab` (switch by name) | real | `switch_tab_article`, `open_url_wiki` |
| `browser_navigate` | `navigate` | real | `links_open_third` (may navigate or click) |
| `browser_read_page` | `getPage` (summaries, answers) | real | `summarize_article`, `read_question` |
| `browser_get_elements` | `getElements` | real | all form cases |
| `browser_click` | `click` (links, radios, checkboxes, submit) | real | `links_open_third`, `form_radio`, `form_submit_*` |
| `browser_fill` | `fill` (text; radio/checkbox via click plan; select/time → skip note) | real | `form_*` |
| `browser_key` | `pressKey` (select via ArrowDown, Enter) | real | `form_select_sauce` |
| `browser_scroll` | `scroll` | real | `scroll_down` |
| `vio::browse` / `open_tab` | `open_url`, `web_search`, `youtube_*` land in Vio | real | search cases |
| Vio chosen while another app is in front (`names_browser`) | — | real | `foreground_calc_fill` |
| `safety::check_step` | Refuse (passwords) > Confirm (send/buy/delete) > Allow | real (confirm answered by case) | `form_submit_denied`, `form_submit_accepted`, `password_refused` |
| YouTube: search + play N-th result | `youtube_search` then click | real (youtube.com) | `youtube_second_result` |
| Forbidden tabs (private / Tor / internal) | task ends, no workaround | — | covered by `vio_mock` unit cases, not voice |

## Not reachable by the talk key (out of this suite)

| Capability | Why | Covered elsewhere |
|---|---|---|
| Circle-to-Ask (screen region + question card) | needs a drag / the Ask hotkey, not a talk-key utterance | manual |
| Island hover UI (now playing, clipboard history, plans, quick apps, quick settings) | pointer UI | `npm run test:ui` (stage logic) |
| Separate-mode hotkeys / double-tap command mode | keyboard hook | `platform/hook.rs` unit tests |
| Wake word „Soul” | offline detector | `npm run test:voice -- --wake` |
| Clipboard history (password skipping) | watcher | `island/clipboard.rs` unit tests |
| Auto-update | network / installer | `updater.rs` |
