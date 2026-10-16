# Graph Report - dynamic island  (2026-10-16)

## Corpus Check
- 123 files · ~169,785 words
- Verdict: corpus is large enough that graph structure adds value.
- Unclassified: 6 file(s) not represented in the graph (top: (none) 2, .toml 1, .icns 1)

## Summary
- 3127 nodes · 7175 edges · 131 communities (113 shown, 18 thin omitted)
- Extraction: 97% EXTRACTED · 3% INFERRED · 0% AMBIGUOUS · INFERRED: 228 edges (avg confidence: 0.86)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `ab072c1e`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- lib.rs
- voice_regression.rs
- gemini.rs
- windows_uia.rs
- windows.rs
- context.rs
- agent/mod.rs
- Island.tsx
- linux_atspi.rs
- windows_hook.rs
- talk.rs
- latest-json.mjs
- router.rs
- updater.rs
- pages.tsx
- Spirit.tsx
- src/config.rs
- safety.rs
- media.rs
- AppearancePage.tsx
- bundle
- UiSnapshot
- exec.rs
- wake.rs
- scenes.rs
- duration
- Settings.tsx
- Action
- agent/intent.rs
- x11_hook.rs
- Platform
- ask.rs
- close.rs
- live.rs
- hook.rs
- run_agent_live
- LiveSession
- memory.rs
- Role
- TalkKey
- linux_x11.rs
- LinuxX11Platform
- AppHost
- island/mod.rs
- killswitch.rs
- confirm.rs
- coords.rs
- voice.rs
- Soul
- computer/intent.rs
- Key
- LinuxWaylandPlatform
- AboutPage.tsx
- text_action.rs
- timers.rs
- WakeEngine
- audio.rs
- clipboard.rs
- ElementOp
- Settings
- platform/mod.rs
- LiveSetup
- AudioRecorder
- computer/mod.rs
- marks.rs
- Actor
- compilerOptions
- prompt.rs
- TextAction
- input.rs
- AppHandle
- capture.rs
- AppHandle
- .push
- package.json
- Host
- CLAUDE.md
- x11_capture.rs
- ParticleField
- .handle_server_message
- Fact
- quota.rs
- build_capture_stream
- .main_loop
- Listener
- scripts
- super
- InputStep
- CTRL
- island_get_snapshot
- progress.ts
- Lang
- runtime
- with_db
- Changelog
- live_smoke.rs
- dependencies
- island_set_mode
- devDependencies
- sounds.ts
- HookConfig
- Combo
- compilerOptions
- Phase
- Activity
- time_fn
- PowerState
- .process
- default.json
- IslandPosition
- vite.config.ts
- albumAccent.ts
- InputProbe
- Endpoint
- instant
- AppHandle
- MouseButton
- tauri.release.conf.json
- Voice regression samples
- Voice regression report
- Voice regression report
- Wake word report
- ci-linux-deps.sh
- soul

## God Nodes (most connected - your core abstractions)
1. `Platform` - 82 edges
2. `load_settings()` - 73 edges
3. `Settings` - 52 edges
4. `useAnim()` - 48 edges
5. `UiSnapshot` - 46 edges
6. `WindowsPlatform` - 40 edges
7. `Role` - 38 edges
8. `LinuxX11Platform` - 36 edges
9. `LinuxWaylandPlatform` - 35 edges
10. `TalkKey` - 33 edges

## Surprising Connections (you probably didn't know these)
- `Visual architecture` --references--> `AppearanceConfig`  [INFERRED]
  CLAUDE.md → src/anim/config.ts
- `Backend – `src-tauri/src/`` --references--> `tr()`  [INFERRED]
  CLAUDE.md → src/i18n.ts
- `Backend – `src-tauri/src/`` --references--> `Settings()`  [INFERRED]
  CLAUDE.md → src/settings/Settings.tsx
- `Models (Gemini)` --references--> `Settings()`  [INFERRED]
  CLAUDE.md → src/settings/Settings.tsx
- `Visual architecture` --references--> `SpiritOrDot()`  [INFERRED]
  CLAUDE.md → src/spirit/Spirit.tsx

## Import Cycles
- 1-file cycle: `src-tauri/src/island/media.rs -> src-tauri/src/island/media.rs`
- 1-file cycle: `src-tauri/src/platform/apps.rs -> src-tauri/src/platform/apps.rs`
- 1-file cycle: `src-tauri/src/platform/linux_x11.rs -> src-tauri/src/platform/linux_x11.rs`
- 2-file cycle: `src-tauri/src/lib.rs -> src-tauri/src/talk.rs -> src-tauri/src/lib.rs`
- 3-file cycle: `src-tauri/src/agent/confirm.rs -> src-tauri/src/lib.rs -> src-tauri/src/talk.rs -> src-tauri/src/agent/confirm.rs`

## Communities (131 total, 18 thin omitted)

### Community 0 - "lib.rs"
Cohesion: 0.05
Nodes (60): on_agent_released(), load_settings(), save_settings(), island_update_pins(), apply_talk_mode(), AppState, ask_cancel(), ask_followup() (+52 more)

### Community 1 - "voice_regression.rs"
Cohesion: 0.05
Nodes (61): CASES_FILE, read_cases(), sample_file_name(), voice_sample_cancel(), voice_sample_start(), voice_sample_stop(), voice_test_dir(), write_cases() (+53 more)

### Community 2 - "gemini.rs"
Cohesion: 0.07
Nodes (53): adapt_thinking_config(), agent_live_system_prompt(), agent_tool_declarations(), agent_voice_system_prompt(), API_BASE, ASK_BACKOFF_SECS, AttemptError, build_agent_body_from_text() (+45 more)

### Community 3 - "windows_uia.rs"
Cohesion: 0.07
Nodes (47): bstr(), CACHED_PROPERTIES, Ctx, element_op(), ensure_dpi_aware_thread(), exclude_from_capture(), foreground_window(), GEN (+39 more)

### Community 4 - "windows.rs"
Cohesion: 0.05
Nodes (27): validate_http_url(), close_app_probe(), CLOSE_GRACE, close_resolve_probe(), CONSOLE_CLASSES, CREATE_NO_WINDOW, ctrl_chord(), duck_sessions() (+19 more)

### Community 5 - "context.rs"
Cohesion: 0.06
Nodes (42): connector_len(), MAX_ACTIONS, split(), split_with(), step_label(), active_block(), begin_turn(), clear() (+34 more)

### Community 6 - "agent/mod.rs"
Cohesion: 0.06
Nodes (51): agent_confirm(), agent_get_config(), AGENT_LIVE_TIMEOUT, agent_save_config(), AgentState, arg_str(), arg_u8(), barge_in() (+43 more)

### Community 7 - "Island.tsx"
Cohesion: 0.08
Nodes (62): Visual architecture, useAnim(), useT(), EqBars(), Marquee(), NextIcon(), PauseIcon(), PlayIcon() (+54 more)

### Community 8 - "linux_atspi.rs"
Cohesion: 0.06
Nodes (47): a11y_bus(), ACCESSIBLE, active_frame(), atom(), BUS, Cache, cached_ref(), call() (+39 more)

### Community 9 - "windows_hook.rs"
Cohesion: 0.04
Nodes (13): ALTGR_FAKE_CTRL_SCAN, handle_mouse(), hook_thread(), install(), INSTALLED, key_input(), keyboard_proc(), mouse_input() (+5 more)

### Community 10 - "talk.rs"
Cohesion: 0.06
Nodes (41): 1. Perception (accessibility first), 2. Action layer, 3. Agent loop (`agent/computer`), 4. Router, 5. Safety, 6. UX, 7. Testing, API calls per task (expected) (+33 more)

### Community 11 - "latest-json.mjs"
Cohesion: 0.05
Nodes (49): body, esc, m, root, text, version, add(), artifacts (+41 more)

### Community 12 - "router.rs"
Cohesion: 0.09
Nodes (51): app_verbs_for_chain_parts(), clean_tail(), cleanup_query(), contains_any(), extract_percent(), extract_web_search_query(), filler_prefix_then_search_keeps_polish_query(), FILLERS (+43 more)

### Community 13 - "updater.rs"
Cohesion: 0.08
Nodes (45): bundle_name(), CHANNEL_BETA, CHANNEL_STABLE, check(), CHECK_INTERVAL, CHECKING, download(), DOWNLOADING (+37 more)

### Community 14 - "pages.tsx"
Cohesion: 0.14
Nodes (49): useAppearance(), tr(), AppearancePage(), comboKey(), HotkeyRecorder(), isFunctionKey(), LABELS, MODIFIER_CODES (+41 more)

### Community 15 - "Spirit.tsx"
Cohesion: 0.07
Nodes (38): react, react-markdown, @tauri-apps/api, Highlight(), App(), AnswerPayload, AskView(), onPointerDown() (+30 more)

### Community 16 - "src/config.rs"
Cohesion: 0.05
Nodes (35): AgentConfig, config_defaults_and_normalization(), load(), LOG, LOG_LIMIT, log_step(), MAX_STEPS_LIMIT, recent_steps() (+27 more)

### Community 17 - "safety.rs"
Cohesion: 0.09
Nodes (39): address_bar(), ALWAYS_BLOCKED_PROCESSES, blocked_by(), blocklist_windows_and_bank_urls(), check_step(), check_window(), ConfirmLevel, Always (+31 more)

### Community 18 - "media.rs"
Cohesion: 0.09
Nodes (28): action(), artists_meta(), attach_session_handlers(), cache(), cached_now_playing(), cached_thumbnail(), fetch(), fetch_inner() (+20 more)

### Community 19 - "AppearancePage.tsx"
Cohesion: 0.08
Nodes (39): framer-motion, AnimationProvider(), AnimCtx, applyCssVars(), Ctx, usePowerSaver(), useReduceMotion(), Anim (+31 more)

### Community 20 - "bundle"
Cohesion: 0.04
Nodes (44): app, security, windows, withGlobalTauri, bundleMediaFramework, build, beforeBuildCommand, beforeDevCommand (+36 more)

### Community 21 - "UiSnapshot"
Cohesion: 0.08
Nodes (21): cap_keeps_focused_and_limits_text(), Caps, el(), finalize_orders_numbers_and_filters(), fingerprint_tracks_visible_changes(), MAX_LABEL_CHARS, MAX_TEXT_ELEMENTS, observation_is_compact_and_safe() (+13 more)

### Community 22 - "exec.rs"
Cohesion: 0.11
Nodes (31): Attempt, can_set_value(), click(), counterpart(), counterpart_survives_reordering(), Cx, default_scroll_target(), element_op() (+23 more)

### Community 23 - "wake.rs"
Cohesion: 0.06
Nodes (31): BAND_SIZE, COOLDOWN, FRAME_LEN, FULLSCREEN_POLL, MAX_CAPTURE_MS, MAX_SAMPLES, MFCC_SIZE, MIC_RETRY (+23 more)

### Community 24 - "scenes.rs"
Cohesion: 0.10
Nodes (31): at(), create_suggested(), find(), fold(), from_args(), levenshtein(), list(), match_trigger() (+23 more)

### Community 25 - "duration"
Cohesion: 0.10
Nodes (23): AppEntry, AppKind, Desktop, Uwp, AppSuggestion, collect_desktop_apps(), collect_lnk_names(), extract_youtube_video_id() (+15 more)

### Community 26 - "Settings.tsx"
Cohesion: 0.09
Nodes (35): catLabel(), EMPTY_SCENE, P, when(), DEFAULT_ASK, DEFAULT_ASK_FALLBACK, DEFAULT_COMMANDS, DEFAULT_DICTATION (+27 more)

### Community 27 - "Action"
Cohesion: 0.10
Nodes (27): Action, AskUser, Click, Done, Drag, FocusApp, KeyCombo, OpenApp (+19 more)

### Community 28 - "agent/intent.rs"
Cohesion: 0.07
Nodes (19): classify(), classify_with(), COMMAND_VERBS, fold_word(), Intent, Command, Dictation, MAX_PREROUTER_WORDS (+11 more)

### Community 29 - "x11_hook.rs"
Cohesion: 0.08
Nodes (24): button_for(), EVENT_LEN, handle_event(), install(), INSTALLED, keycode_has(), keysym_for(), RECORD_FROM_SERVER (+16 more)

### Community 31 - "ask.rs"
Cohesion: 0.12
Nodes (21): AskAnswerPayload, AskSession, AskSessionPayload, AskStatusPayload, cancel_ask(), ChatTurn, followup_ask(), hide_ask_window() (+13 more)

### Community 32 - "close.rs"
Cohesion: 0.15
Nodes (27): app(), CloseTarget, desktop(), discord_resolves_to_root_by_exe_not_title(), display_name(), fold(), is_protected(), KNOWN (+19 more)

### Community 33 - "live.rs"
Cohesion: 0.08
Nodes (18): redact_secrets(), BACKOFF_MS, collect_dictation(), describe_ws_error(), DICTATION_HARD_TIMEOUT, DICTATION_QUIET_GAP, DICTATION_SETTLED_WAIT, DictationResult (+10 more)

### Community 34 - "hook.rs"
Cohesion: 0.14
Nodes (19): altgr_blocks_talk_press_and_disabled_passes(), chord_cancels_and_passes_through(), core(), Decision, drag_gesture_swallows_left_button(), HookCore, HookEvent, Chord (+11 more)

### Community 35 - "run_agent_live"
Cohesion: 0.18
Nodes (23): auto_remember(), CommandOutcome, computer_command(), drain_voice(), finish_command(), handle_live_calls(), LiveAgentOutcome, Computer (+15 more)

### Community 36 - "LiveSession"
Cohesion: 0.12
Nodes (11): Cmd, Audio, Begin, BeginText, Cancel, Configure, Disable, End (+3 more)

### Community 37 - "memory.rs"
Cohesion: 0.13
Nodes (22): add_fact(), all_facts(), CATEGORIES, clear_all(), DB, DB_FILE, dismiss(), extract_fact() (+14 more)

### Community 38 - "Role"
Cohesion: 0.08
Nodes (21): finalize(), Role, Button, CheckBox, ComboBox, DataItem, Document, Edit (+13 more)

### Community 39 - "TalkKey"
Cohesion: 0.09
Nodes (22): TalkKey, AltLeft, CapsLock, ContextMenu, ControlLeft, ControlRight, F, Insert (+14 more)

### Community 40 - "linux_x11.rs"
Cohesion: 0.12
Nodes (14): alive(), close_target(), DUCKED, FULLSCREEN_PROBE, FullscreenProbe, own_stream_is_identifiable_for_skipping(), pactl_duck(), parse_sink_inputs() (+6 more)

### Community 41 - "LinuxX11Platform"
Cohesion: 0.08
Nodes (3): linux_do_not_disturb(), LinuxX11Platform, read_volume()

### Community 42 - "AppHost"
Cohesion: 0.10
Nodes (3): AppHost, OWN_WINDOWS, Progress

### Community 43 - "island/mod.rs"
Cohesion: 0.08
Nodes (19): CaptionPayload, COLLAPSED_H, COLLAPSED_W, cursor_over_panel(), cursor_screen_pos(), EXPANDED_H, EXPANDED_W, HARD_COLLAPSE_MS (+11 more)

### Community 44 - "killswitch.rs"
Cohesion: 0.12
Nodes (18): abort(), ABORTED, CORNER_HOLD, CORNER_MARGIN, corner_needs_a_short_hold(), Detector, DOUBLE_ESC, double_escape_aborts_single_does_not() (+10 more)

### Community 45 - "confirm.rs"
Cohesion: 0.12
Nodes (15): answer(), ANSWERING, ask(), begin_answer(), blocks_overlay(), CONFIRM_TIMEOUT, is_pending(), mark_answering() (+7 more)

### Community 46 - "coords.rs"
Cohesion: 0.24
Nodes (23): clamp_off_corners(), downscaled_screenshot_pixels_map_back(), dpi_100_125_150_logical_physical(), from_absolute(), hard_corners_only_where_the_cursor_stops(), image_to_screen(), in_hard_corner(), layout() (+15 more)

### Community 47 - "voice.rs"
Cohesion: 0.12
Nodes (11): emit_speaking(), IDLE_CLOSE, LEVEL_EVERY, Output, PlayerCmd, Attach, Push, REPLY_SAMPLE_RATE (+3 more)

### Community 48 - "Soul"
Cohesion: 0.08
Nodes (23): All platforms, Architecture, Build, Code signing (later, optional), Develop, Dynamic Island — how to test, Hotkeys (defaults), Install (+15 more)

### Community 49 - "computer/intent.rs"
Cohesion: 0.12
Nodes (18): btn(), CLICK_VERBS, IN_APP_NOUNS, is_computer_task(), is_explicit_ui_command(), local_click_plan_needs_exactly_one_match(), local_plan(), local_scroll_plan() (+10 more)

### Community 50 - "Key"
Cohesion: 0.09
Nodes (23): Key, Alt, Backspace, Char, Ctrl, Delete, Down, End (+15 more)

### Community 52 - "AboutPage.tsx"
Cohesion: 0.15
Nodes (17): Backend – `src-tauri/src/`, holdsCard(), mustCollapseFor(), pickStage(), Stage, StageInput, formatBytes(), hasPendingUpdate() (+9 more)

### Community 53 - "text_action.rs"
Cohesion: 0.11
Nodes (14): CASUAL, EXTRA, FIX_OBJECTS, FORMAL, Info, LANGUAGES, LAST_INFO, LAST_UNDO (+6 more)

### Community 54 - "timers.rs"
Cohesion: 0.18
Nodes (15): add(), broadcast(), cancel(), fire(), list(), MAX_SECONDS, NEXT_ID, now_ms() (+7 more)

### Community 55 - "WakeEngine"
Cohesion: 0.17
Nodes (13): build_model(), Detection, detector_config(), dither(), engine_detects_its_own_template_and_ignores_noise(), leave_one_out(), new_detector(), probe_config() (+5 more)

### Community 56 - "audio.rs"
Cohesion: 0.12
Nodes (11): encode_wav_16k_mono(), LEVEL_EMIT_EVERY, list_microphones(), MicLevelPayload, MicrophoneInfo, PCM_CHUNK_SAMPLES, resample_linear(), resolve_input_device() (+3 more)

### Community 57 - "clipboard.rs"
Cohesion: 0.16
Nodes (12): clear(), ClipboardItem, ClipboardState, copy_item(), emit_clipboard(), HISTORY_LIMIT, list(), looks_like_secret() (+4 more)

### Community 58 - "ElementOp"
Cohesion: 0.10
Nodes (15): pattern_click(), ElementOp, Collapse, Expand, Focus, Invoke, Scroll, Select (+7 more)

### Community 59 - "Settings"
Cohesion: 0.13
Nodes (14): context_active(), context_block(), default_update_channel(), IslandSections, redact_settings(), Settings, get_settings(), Heard (+6 more)

### Community 60 - "platform/mod.rs"
Cohesion: 0.14
Nodes (10): create_platform(), is_wayland(), native_window_id(), restore(), save(), Saved, Empty, Image (+2 more)

### Community 61 - "LiveSetup"
Cohesion: 0.15
Nodes (12): commands_setup(), commands_setup_pins_polish_reply_language(), connect(), connect_once(), dictation_setup(), language_codes(), LiveKind, Commands (+4 more)

### Community 62 - "AudioRecorder"
Cohesion: 0.14
Nodes (8): AudioCmd, IsRecording, Start, StartMonitor, Stop, StopMonitor, AudioRecorder, StartResult

### Community 63 - "computer/mod.rs"
Cohesion: 0.11
Nodes (8): HIGHLIGHT_LABEL, HIGHLIGHT_PAD, HIGHLIGHT_VISIBLE, MAX_FAILS_IN_ROW, show_highlight(), STEP_TIMEOUT, TASK_TIMEOUT, TOOL_NAME

### Community 64 - "marks.rs"
Cohesion: 0.23
Nodes (14): fit_within(), crops_scales_marks_and_encodes(), DIGITS, draw_box(), draw_label(), encode_jpeg(), JPEG_QUALITY, labels_stay_inside_the_image() (+6 more)

### Community 65 - "Actor"
Cohesion: 0.29
Nodes (3): Actor, soul_live_log(), Turn

### Community 66 - "compilerOptions"
Cohesion: 0.11
Nodes (18): compilerOptions, allowImportingTsExtensions, isolatedModules, jsx, lib, module, moduleResolution, noEmit (+10 more)

### Community 67 - "prompt.rs"
Cohesion: 0.21
Nodes (11): common_props(), decl(), first_call(), function_declarations(), Observation, request_body(), request_has_one_turn_screen_block_and_optional_image(), snap() (+3 more)

### Community 68 - "TextAction"
Cohesion: 0.17
Nodes (11): parse(), run(), run_inner(), TextAction, Casual, Expand, Explain, Fix (+3 more)

### Community 69 - "input.rs"
Cohesion: 0.21
Nodes (16): ABORTED, emoji_become_surrogate_pairs_and_newlines_are_enter(), execute(), execute_releases_everything_on_abort_and_error(), KeyDown, KeyUp, plan_combo(), plan_type() (+8 more)

### Community 70 - "AppHandle"
Cohesion: 0.22
Nodes (14): apply(), Config, Config, init(), record_sample(), sample_files(), samples_dir(), set_config() (+6 more)

### Community 71 - "capture.rs"
Cohesion: 0.20
Nodes (8): capture_monitor_under_cursor(), CaptureResult, crop_png(), CropRect, cursor_position(), grab_monitor_at(), is_wayland(), rgba_to_png()

### Community 72 - "AppHandle"
Cohesion: 0.16
Nodes (13): emit_assistant_state(), emit_caption(), emit_tool(), emit_tool_labeled(), ensure_window_geometry(), island_clipboard_clear(), island_list_apps(), island_search_apps() (+5 more)

### Community 73 - ".push"
Cohesion: 0.18
Nodes (8): Decimator, decimator_keeps_duration_and_dc(), Endpointer, endpointer_caps_and_flags_no_speech(), endpointer_stops_after_silence(), MicTap, read_wav_16k(), wake_cpu_live()

### Community 74 - "package.json"
Cohesion: 0.13
Nodes (14): name, private, type, version, react-dom, tailwindcss, @tauri-apps/cli, @tauri-apps/plugin-global-shortcut (+6 more)

### Community 75 - "Host"
Cohesion: 0.20
Nodes (3): ExclusionGuard, Host, run_task()

### Community 76 - "CLAUDE.md"
Cohesion: 0.14
Nodes (12): Architecture, Frontend – `src/`, graphify, Hotkeys (defaults from `config.rs`), Island behaviour, Logging conventions, Models (Gemini), Packaging / releases (+4 more)

### Community 77 - "x11_capture.rs"
Cohesion: 0.25
Nodes (7): converts_bgrx_and_xrgb_with_padding(), grab_monitor_at(), MonitorRect, pick_monitor(), picks_monitor_under_cursor_then_primary(), rect(), zpixmap32_to_rgba()

### Community 78 - "ParticleField"
Cohesion: 0.19
Nodes (5): drawStar(), Emitter, P, ParticleField, roundRect()

### Community 79 - ".handle_server_message"
Cohesion: 0.22
Nodes (11): audio_part(), parse_function_calls(), TurnEvent, Audio, Complete, Error, Interim, OutputTranscript (+3 more)

### Community 80 - "Fact"
Cohesion: 0.24
Nodes (14): Fact, format_prompt(), list(), live_prompt_order_ignores_usage(), now_ms(), prompt_block_is_bounded_and_labelled(), PROMPT_FACTS, rank() (+6 more)

### Community 81 - "quota.rs"
Cohesion: 0.23
Nodes (3): map(), mark_daily_exhausted(), next_pacific_midnight_utc()

### Community 82 - "build_capture_stream"
Cohesion: 0.22
Nodes (6): build_capture_stream(), level_emitter(), pcm_bytes(), push_mono(), run_pcm_feeder(), update_level()

### Community 83 - ".main_loop"
Cohesion: 0.33
Nodes (3): Outcome, Run, clip()

### Community 85 - "scripts"
Cohesion: 0.17
Nodes (12): scripts, build, changelog, dev, preview, release, tauri, test:ui (+4 more)

### Community 86 - "super"
Cohesion: 0.20
Nodes (5): capitalize_first(), CORRECTION_MARKERS, FILLERS, is_filler(), local_cleanup()

### Community 87 - "InputStep"
Cohesion: 0.36
Nodes (11): click_scroll_drag_plans(), InputStep, ButtonDown, ButtonUp, MoveTo, Pause, Unicode, Wheel (+3 more)

### Community 88 - "CTRL"
Cohesion: 0.20
Nodes (7): CTRL, Reload, Test, DIR, send(), TRAY_ITEM, wake_test()

### Community 89 - "island_get_snapshot"
Cohesion: 0.20
Nodes (8): clock_string(), current_plans(), island_clipboard_list(), island_get_now_playing(), island_get_snapshot(), IslandSnapshot, PlanItem, refresh_plans()

### Community 90 - "progress.ts"
Cohesion: 0.38
Nodes (10): clamp(), fresh(), monotonic(), positionAt(), rateOf(), reconcile(), RESYNC_DRIFT_MS, sourceAt() (+2 more)

### Community 91 - "Lang"
Cohesion: 0.27
Nodes (8): done_text(), cancelled_message(), close_question(), Lang, En, Pl, tr(), ui_lang()

### Community 92 - "runtime"
Cohesion: 0.31
Nodes (9): apply_enabled(), apply_geometry(), clear_assistant(), hard_collapse(), init(), prepare_transparent_window(), runtime(), start_leave_watcher() (+1 more)

### Community 93 - "with_db"
Cohesion: 0.20
Nodes (10): count(), delete_fact(), forget(), is_dismissed(), keyword_hits(), launches_since(), stem(), tokens() (+2 more)

### Community 94 - "Changelog"
Cohesion: 0.20
Nodes (9): [0.1.0] - 2026-10-15, [0.2.0] - 2026-10-15, [0.2.1] - 2026-10-15, Added, Added, Changelog, Fixed, Fixed (+1 more)

### Community 95 - "live_smoke.rs"
Cohesion: 0.31
Nodes (6): key(), live_smoke(), stream(), TTS_MODEL, tts_pcm16k(), tts_pcm16k_styled()

### Community 96 - "dependencies"
Cohesion: 0.20
Nodes (10): dependencies, framer-motion, react, react-dom, react-markdown, @tauri-apps/api, @tauri-apps/plugin-global-shortcut, @tauri-apps/plugin-process (+2 more)

### Community 97 - "island_set_mode"
Cohesion: 0.24
Nodes (6): island_set_expanded(), island_set_mode(), IslandMode, Collapsed, Expanded, Peek

### Community 98 - "devDependencies"
Cohesion: 0.22
Nodes (9): devDependencies, tailwindcss, @tailwindcss/vite, @tauri-apps/cli, @types/react, @types/react-dom, typescript, vite (+1 more)

### Community 99 - "sounds.ts"
Cohesion: 0.25
Nodes (6): Cue, FUTURISTIC, playCue(), SoundPack, SUBTLE, Tone

### Community 100 - "HookConfig"
Cohesion: 0.25
Nodes (3): HookConfig, configure(), configure()

### Community 101 - "Combo"
Cohesion: 0.25
Nodes (3): Combo, combo_plan_releases_in_reverse(), combos_parse_and_normalize()

### Community 102 - "compilerOptions"
Cohesion: 0.25
Nodes (7): compilerOptions, allowSyntheticDefaultImports, composite, module, moduleResolution, skipLibCheck, include

### Community 103 - "Phase"
Cohesion: 0.29
Nodes (7): Phase, Aborted, Asking, Confirm, Done, Failed, Running

### Community 104 - "Activity"
Cohesion: 0.29
Nodes (7): Activity, Agent, Command, Dictation, AssistantBanner, IslandRuntime, set_activity()

### Community 105 - "time_fn"
Cohesion: 0.29
Nodes (5): island_clipboard_copy(), island_launch_pinned(), island_media_action(), island_set_volume(), time_fn()

### Community 106 - "PowerState"
Cohesion: 0.29
Nodes (3): system_power_state(), sysfs_on_battery(), PowerState

### Community 107 - ".process"
Cohesion: 0.43
Nodes (3): Resampler, resampler_doubles_rate_without_gaps(), resampler_identity_rate()

### Community 108 - "default.json"
Cohesion: 0.33
Nodes (5): description, identifier, permissions, $schema, windows

### Community 109 - "IslandPosition"
Cohesion: 0.40
Nodes (4): IslandPosition, TopCenter, TopLeft, TopRight

### Community 110 - "vite.config.ts"
Cohesion: 0.40
Nodes (3): @tailwindcss/vite, vite, @vitejs/plugin-react

### Community 111 - "albumAccent.ts"
Cohesion: 0.70
Nodes (4): dominant(), hslToHex(), rgbToHsl(), useAlbumAccent()

### Community 114 - "Endpoint"
Cohesion: 0.40
Nodes (5): Endpoint, Continue, MaxLength, NoSpeech, Silence

### Community 116 - "AppHandle"
Cohesion: 0.83
Nodes (3): platform_of(), text_action_undo(), text_result_paste()

### Community 117 - "MouseButton"
Cohesion: 0.50
Nodes (4): MouseButton, Left, Middle, Right

### Community 118 - "tauri.release.conf.json"
Cohesion: 0.50
Nodes (3): bundle, createUpdaterArtifacts, $schema

## Knowledge Gaps
- **164 isolated node(s):** `@tauri-apps/plugin-global-shortcut`, `@tauri-apps/plugin-store`, `@tauri-apps/plugin-updater`, `@tauri-apps/cli`, `@types/react` (+159 more)
  These have ≤1 connection - possible missing edges or undocumented components. (Counts symbols only; 1106 node(s) total have ≤1 connection when file, concept and rationale nodes are included.)
- **18 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `resolve_fallback_model()` connect `Settings` to `lib.rs`, `gemini.rs`, `run_agent_live`, `TextAction`, `agent/mod.rs`, `CLAUDE.md`, `text_action.rs`, `ask.rs`?**
  _High betweenness centrality (0.212) - this node is a cross-community bridge._
- **Are the 53 inferred relationships involving `load_settings()` (e.g. with `.new()` and `on_answer_released()`) actually correct?**
  _`load_settings()` has 53 INFERRED edges - model-reasoned connections that need verification._
- **What connects `@tauri-apps/plugin-global-shortcut`, `@tauri-apps/plugin-store`, `@tauri-apps/plugin-updater` to the rest of the system?**
  _164 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `lib.rs` be split into smaller, more focused modules?**
  _Cohesion score 0.04725085910652921 - nodes in this community are weakly interconnected._
- **Why does `Models (Gemini)` connect `CLAUDE.md` to `Settings`, `pages.tsx`?**
  _High betweenness centrality (0.211) - this node is a cross-community bridge._
- **Should `voice_regression.rs` be split into smaller, more focused modules?**
  _Cohesion score 0.050888286660644384 - nodes in this community are weakly interconnected._
- **Why does `Settings()` connect `pages.tsx` to `Island.tsx`, `CLAUDE.md`, `Spirit.tsx`, `AboutPage.tsx`, `Settings.tsx`?**
  _High betweenness centrality (0.189) - this node is a cross-community bridge._