    fn snapshot_composer_state_with_width<F>(
        name: &str,
        width: u16,
        enhanced_keys_supported: bool,
        setup: F,
    ) where
        F: FnOnce(&mut ChatComposer),
    {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            enhanced_keys_supported,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        setup(&mut composer);
        let footer_props = composer.footer_props();
        let footer_lines = footer_height(footer_props);
        let footer_spacing = ChatComposer::footer_spacing(footer_lines);
        let height = footer_lines + footer_spacing + 8;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| composer.render(f.area(), f.buffer_mut()))
            .unwrap();
        insta::assert_snapshot!(name, terminal.backend());
    }

    #[test]
    fn alt_screen_hides_decorative_left_border() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            true,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_alt_screen_active(true);
        composer.set_session_mode(true);

        let area = Rect::new(0, 0, 60, 8);
        let mut buf = Buffer::empty(area);
        composer.render(area, &mut buf);

        let left_col: String = (0..area.height)
            .map(|y| buf[(0, y)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            !left_col.contains('┃') && !left_col.contains('╹'),
            "decorative border leaked in alt-screen mode: {left_col:?}"
        );
    }

    fn snapshot_composer_state<F>(name: &str, enhanced_keys_supported: bool, setup: F)
    where
        F: FnOnce(&mut ChatComposer),
    {
        snapshot_composer_state_with_width(name, 100, enhanced_keys_supported, setup);
    }

    #[test]
    fn footer_mode_snapshots() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        snapshot_composer_state("footer_mode_shortcut_overlay", true, |composer| {
            composer.set_esc_backtrack_hint(true);
            let _ =
                composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        });

        snapshot_composer_state("footer_mode_ctrl_c_quit", true, |composer| {
            composer.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')), true);
        });

        snapshot_composer_state("footer_mode_ctrl_c_interrupt", true, |composer| {
            composer.set_task_running(true);
            composer.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')), true);
        });

        snapshot_composer_state("footer_mode_ctrl_c_then_esc_hint", true, |composer| {
            composer.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')), true);
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        });

        snapshot_composer_state("footer_mode_esc_hint_from_overlay", true, |composer| {
            let _ =
                composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        });

        snapshot_composer_state("footer_mode_esc_hint_backtrack", true, |composer| {
            composer.set_esc_backtrack_hint(true);
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        });

        snapshot_composer_state(
            "footer_mode_overlay_then_external_esc_hint",
            true,
            |composer| {
                let _ = composer
                    .handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
                composer.set_esc_backtrack_hint(true);
            },
        );

        snapshot_composer_state("footer_mode_hidden_while_typing", true, |composer| {
            type_chars_humanlike(composer, &['h']);
        });
    }

    #[test]
    fn footer_collapse_snapshots() {
        fn setup_collab_footer(
            composer: &mut ChatComposer,
            context_percent: i64,
            indicator: Option<CollaborationModeIndicator>,
        ) {
            composer.set_collaboration_modes_enabled(true);
            composer.set_collaboration_mode_indicator(indicator);
            composer.set_context_window(Some(context_percent), None);
        }

        // Empty textarea, agent idle: shortcuts hint can show, and cycle hint is hidden.
        snapshot_composer_state_with_width("footer_collapse_empty_full", 120, true, |composer| {
            setup_collab_footer(composer, 100, None);
        });
        snapshot_composer_state_with_width(
            "footer_collapse_empty_mode_cycle_with_context",
            60,
            true,
            |composer| {
                setup_collab_footer(composer, 100, None);
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_empty_mode_cycle_without_context",
            44,
            true,
            |composer| {
                setup_collab_footer(composer, 100, None);
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_empty_mode_only",
            26,
            true,
            |composer| {
                setup_collab_footer(composer, 100, None);
            },
        );

        // Empty textarea, plan mode idle: shortcuts hint and cycle hint are available.
        snapshot_composer_state_with_width(
            "footer_collapse_plan_empty_full",
            120,
            true,
            |composer| {
                setup_collab_footer(composer, 100, Some(CollaborationModeIndicator::Plan));
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_plan_empty_mode_cycle_with_context",
            60,
            true,
            |composer| {
                setup_collab_footer(composer, 100, Some(CollaborationModeIndicator::Plan));
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_plan_empty_mode_cycle_without_context",
            44,
            true,
            |composer| {
                setup_collab_footer(composer, 100, Some(CollaborationModeIndicator::Plan));
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_plan_empty_mode_only",
            26,
            true,
            |composer| {
                setup_collab_footer(composer, 100, Some(CollaborationModeIndicator::Plan));
            },
        );

        // Textarea has content, agent running, steer enabled: queue hint is shown.
        snapshot_composer_state_with_width("footer_collapse_queue_full", 120, true, |composer| {
            setup_collab_footer(composer, 98, None);
            composer.set_steer_enabled(true);
            composer.set_task_running(true);
            composer.set_text_content("Test".to_owned(), Vec::new(), Vec::new());
        });
        snapshot_composer_state_with_width(
            "footer_collapse_queue_short_with_context",
            50,
            true,
            |composer| {
                setup_collab_footer(composer, 98, None);
                composer.set_steer_enabled(true);
                composer.set_task_running(true);
                composer.set_text_content("Test".to_owned(), Vec::new(), Vec::new());
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_queue_message_without_context",
            40,
            true,
            |composer| {
                setup_collab_footer(composer, 98, None);
                composer.set_steer_enabled(true);
                composer.set_task_running(true);
                composer.set_text_content("Test".to_owned(), Vec::new(), Vec::new());
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_queue_short_without_context",
            30,
            true,
            |composer| {
                setup_collab_footer(composer, 98, None);
                composer.set_steer_enabled(true);
                composer.set_task_running(true);
                composer.set_text_content("Test".to_owned(), Vec::new(), Vec::new());
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_queue_mode_only",
            20,
            true,
            |composer| {
                setup_collab_footer(composer, 98, None);
                composer.set_steer_enabled(true);
                composer.set_task_running(true);
                composer.set_text_content("Test".to_owned(), Vec::new(), Vec::new());
            },
        );

        // Textarea has content, plan mode active, agent running, steer enabled: queue hint + mode.
        snapshot_composer_state_with_width(
            "footer_collapse_plan_queue_full",
            120,
            true,
            |composer| {
                setup_collab_footer(composer, 98, Some(CollaborationModeIndicator::Plan));
                composer.set_steer_enabled(true);
                composer.set_task_running(true);
                composer.set_text_content("Test".to_owned(), Vec::new(), Vec::new());
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_plan_queue_short_with_context",
            50,
            true,
            |composer| {
                setup_collab_footer(composer, 98, Some(CollaborationModeIndicator::Plan));
                composer.set_steer_enabled(true);
                composer.set_task_running(true);
                composer.set_text_content("Test".to_owned(), Vec::new(), Vec::new());
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_plan_queue_message_without_context",
            40,
            true,
            |composer| {
                setup_collab_footer(composer, 98, Some(CollaborationModeIndicator::Plan));
                composer.set_steer_enabled(true);
                composer.set_task_running(true);
                composer.set_text_content("Test".to_owned(), Vec::new(), Vec::new());
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_plan_queue_short_without_context",
            30,
            true,
            |composer| {
                setup_collab_footer(composer, 98, Some(CollaborationModeIndicator::Plan));
                composer.set_steer_enabled(true);
                composer.set_task_running(true);
                composer.set_text_content("Test".to_owned(), Vec::new(), Vec::new());
            },
        );
        snapshot_composer_state_with_width(
            "footer_collapse_plan_queue_mode_only",
            20,
            true,
            |composer| {
                setup_collab_footer(composer, 98, Some(CollaborationModeIndicator::Plan));
                composer.set_steer_enabled(true);
                composer.set_task_running(true);
                composer.set_text_content("Test".to_owned(), Vec::new(), Vec::new());
            },
        );
    }

    #[test]
    fn esc_hint_stays_hidden_with_draft_content() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            true,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        type_chars_humanlike(&mut composer, &['d']);

        assert!(!composer.is_empty());
        assert_eq!(composer.current_text(), "d");
        assert_eq!(composer.footer_mode, FooterMode::ComposerEmpty);
        assert!(matches!(composer.active_popup, ActivePopup::None));

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(composer.footer_mode, FooterMode::ComposerEmpty);
        assert!(!composer.esc_backtrack_hint);
    }

    #[test]
    fn base_footer_mode_tracks_empty_state_after_quit_hint_expires() {
        use crossterm::event::KeyCode;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        type_chars_humanlike(&mut composer, &['d']);
        composer.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')), true);
        composer.quit_shortcut_expires_at =
            Some(Instant::now() - std::time::Duration::from_secs(1));

        assert_eq!(composer.footer_mode(), FooterMode::ComposerHasDraft);

        composer.set_text_content(String::new(), Vec::new(), Vec::new());
        assert_eq!(composer.footer_mode(), FooterMode::ComposerEmpty);
    }

    #[test]
    fn clear_for_ctrl_c_records_cleared_draft() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);

        composer.set_text_content("draft text".to_owned(), Vec::new(), Vec::new());
        assert_eq!(composer.clear_for_ctrl_c(), Some("draft text".to_owned()));
        assert!(composer.is_empty());

        assert_eq!(
            composer.history.navigate_up(&composer.app_event_tx),
            Some(HistoryEntry::from_text("draft text".to_owned()))
        );
    }

    /// Behavior: `?` toggles the shortcut overlay only when the composer is otherwise empty. After
    /// any typing has occurred, `?` should be inserted as a literal character.
    #[test]
    fn question_mark_only_toggles_on_first_char() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);

        let (result, needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(result, InputResult::None);
        assert!(needs_redraw, "toggling overlay should request redraw");
        assert_eq!(composer.footer_mode, FooterMode::ShortcutOverlay);

        // Toggle back to prompt mode so subsequent typing captures characters.
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(composer.footer_mode, FooterMode::ComposerEmpty);

        type_chars_humanlike(&mut composer, &['h']);
        assert_eq!(composer.textarea.text(), "h");
        assert_eq!(composer.footer_mode(), FooterMode::ComposerHasDraft);

        let (result, needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(result, InputResult::None);
        assert!(needs_redraw, "typing should still mark the view dirty");
        let _ = flush_after_paste_burst(&mut composer);
        assert_eq!(composer.textarea.text(), "h?");
        assert_eq!(composer.footer_mode, FooterMode::ComposerEmpty);
        assert_eq!(composer.footer_mode(), FooterMode::ComposerHasDraft);
    }

    /// Behavior: while a paste-like burst is being captured, `?` must not toggle the shortcut
    /// overlay; it should be treated as part of the pasted content.
    #[test]
    fn question_mark_does_not_toggle_during_paste_burst() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);

        // Force an active paste burst so this test doesn't depend on tight timing.
        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        for ch in ['h', 'i', '?', 't', 'h', 'e', 'r', 'e'] {
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert!(composer.is_in_paste_burst());
        assert_eq!(composer.textarea.text(), "");

        let _ = flush_after_paste_burst(&mut composer);

        assert_eq!(composer.textarea.text(), "hi?there");
        assert_ne!(composer.footer_mode, FooterMode::ShortcutOverlay);
    }

    #[test]
    fn shortcut_overlay_persists_while_task_running() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(composer.footer_mode, FooterMode::ShortcutOverlay);

        composer.set_task_running(true);

        assert_eq!(composer.footer_mode, FooterMode::ShortcutOverlay);
        assert_eq!(composer.footer_mode(), FooterMode::ShortcutOverlay);
    }

    #[test]
    fn test_current_at_token_basic_cases() {
        let test_cases = vec![
            // Valid @ tokens
            ("@hello", 3, Some("hello".to_owned()), "Basic ASCII token"),
            (
                "@file.txt",
                4,
                Some("file.txt".to_owned()),
                "ASCII with extension",
            ),
            (
                "hello @world test",
                8,
                Some("world".to_owned()),
                "ASCII token in middle",
            ),
            (
                "@test123",
                5,
                Some("test123".to_owned()),
                "ASCII with numbers",
            ),
            // Unicode examples
            ("@İstanbul", 3, Some("İstanbul".to_owned()), "Turkish text"),
            (
                "@testЙЦУ.rs",
                8,
                Some("testЙЦУ.rs".to_owned()),
                "Mixed ASCII and Cyrillic",
            ),
            ("@诶", 2, Some("诶".to_owned()), "Chinese character"),
            ("@👍", 2, Some("👍".to_owned()), "Emoji token"),
            // Invalid cases (should return None)
            ("hello", 2, None, "No @ symbol"),
            (
                "@",
                1,
                Some("".to_owned()),
                "Only @ symbol triggers empty query",
            ),
            ("@ hello", 2, None, "@ followed by space"),
            ("test @ world", 6, None, "@ with spaces around"),
        ];

        for (input, cursor_pos, expected, description) in test_cases {
            let mut textarea = TextArea::new();
            textarea.insert_str(input);
            textarea.set_cursor(cursor_pos);

            let result = ChatComposer::current_at_token(&textarea);
            assert_eq!(
                result, expected,
                "Failed for case: {description} - input: '{input}', cursor: {cursor_pos}"
            );
        }
    }

    #[test]
    fn test_current_at_token_cursor_positions() {
        let test_cases = vec![
            // Different cursor positions within a token
            ("@test", 0, Some("test".to_owned()), "Cursor at @"),
            ("@test", 1, Some("test".to_owned()), "Cursor after @"),
            ("@test", 5, Some("test".to_owned()), "Cursor at end"),
            // Multiple tokens - cursor determines which token
            ("@file1 @file2", 0, Some("file1".to_owned()), "First token"),
            (
                "@file1 @file2",
                8,
                Some("file2".to_owned()),
                "Second token",
            ),
            // Edge cases
            ("@", 0, Some("".to_owned()), "Only @ symbol"),
            ("@a", 2, Some("a".to_owned()), "Single character after @"),
            ("", 0, None, "Empty input"),
        ];

        for (input, cursor_pos, expected, description) in test_cases {
            let mut textarea = TextArea::new();
            textarea.insert_str(input);
            textarea.set_cursor(cursor_pos);

            let result = ChatComposer::current_at_token(&textarea);
            assert_eq!(
                result, expected,
                "Failed for cursor position case: {description} - input: '{input}', cursor: {cursor_pos}",
            );
        }
    }

    #[test]
    fn test_current_at_token_whitespace_boundaries() {
        let test_cases = vec![
            // Space boundaries
            (
                "aaa@aaa",
                4,
                None,
                "Connected @ token - no completion by design",
            ),
            (
                "aaa @aaa",
                5,
                Some("aaa".to_owned()),
                "@ token after space",
            ),
            (
                "test @file.txt",
                7,
                Some("file.txt".to_owned()),
                "@ token after space",
            ),
            // Full-width space boundaries
            (
                "test　@İstanbul",
                8,
                Some("İstanbul".to_owned()),
                "@ token after full-width space",
            ),
            (
                "@ЙЦУ　@诶",
                10,
                Some("诶".to_owned()),
                "Full-width space between Unicode tokens",
            ),
            // Tab and newline boundaries
            (
                "test\t@file",
                6,
                Some("file".to_owned()),
                "@ token after tab",
            ),
        ];

        for (input, cursor_pos, expected, description) in test_cases {
            let mut textarea = TextArea::new();
            textarea.insert_str(input);
            textarea.set_cursor(cursor_pos);

            let result = ChatComposer::current_at_token(&textarea);
            assert_eq!(
                result, expected,
                "Failed for whitespace boundary case: {description} - input: '{input}', cursor: {cursor_pos}",
            );
        }
    }

    /// Behavior: if the ASCII path has a pending first char (flicker suppression) and a non-ASCII
    /// char arrives next, the pending ASCII char should still be preserved and the overall input
    /// should submit normally (i.e. we should not misclassify this as a paste burst).
    #[test]
    fn ascii_prefix_survives_non_ascii_followup() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        assert!(composer.is_in_paste_burst());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('あ'), KeyModifiers::NONE));

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted { text, .. } => assert_eq!(text, "1あ"),
            _ => panic!("expected Submitted"),
        }
    }

    /// Behavior: a single non-ASCII char should be inserted immediately (IME-friendly) and should
    /// not create any paste-burst state.
    #[test]
    fn non_ascii_char_inserts_immediately_without_burst_state() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('あ'), KeyModifiers::NONE));

        assert_eq!(composer.textarea.text(), "あ");
        assert!(!composer.is_in_paste_burst());
    }

    /// Behavior: while we're capturing a paste-like burst, Enter should be treated as a newline
    /// within the burst (not as "submit"), and the whole payload should flush as one paste.
    #[test]
    fn non_ascii_burst_buffers_enter_and_flushes_multiline() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));

        assert!(composer.textarea.text().is_empty());
        let _ = flush_after_paste_burst(&mut composer);
        assert_eq!(composer.textarea.text(), "你好\nhi");
    }

    /// Behavior: a paste-like burst may include a full-width/ideographic space (U+3000). It should
    /// still be captured as a single paste payload and preserve the exact Unicode content.
    #[test]
    fn non_ascii_burst_preserves_ideographic_space_and_ascii() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        for ch in ['你', '　', '好'] {
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for ch in ['h', 'i'] {
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        assert!(composer.textarea.text().is_empty());
        let _ = flush_after_paste_burst(&mut composer);
        assert_eq!(composer.textarea.text(), "你　好\nhi");
    }

    /// Behavior: a large multi-line payload containing both non-ASCII and ASCII (e.g. "UTF-8",
    /// "Unicode") should be captured as a single paste-like burst, and Enter key events should
    /// become `\n` within the buffered content.
    #[test]
    fn non_ascii_burst_buffers_large_multiline_mixed_ascii_and_unicode() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        const LARGE_MIXED_PAYLOAD: &str = "天地玄黄 宇宙洪荒\n\
日月盈昃 辰宿列张\n\
寒来暑往 秋收冬藏\n\
\n\
你好世界 编码测试\n\
汉字处理 UTF-8\n\
终端显示 正确无误\n\
\n\
风吹竹林 月照大江\n\
白云千载 青山依旧\n\
程序员 与 Unicode 同行";

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        // Force an active burst so the test doesn't depend on timing heuristics.
        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        for ch in LARGE_MIXED_PAYLOAD.chars() {
            let code = if ch == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(ch)
            };
            let _ = composer.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
        }

        assert!(composer.textarea.text().is_empty());
        let _ = flush_after_paste_burst(&mut composer);
        assert_eq!(composer.textarea.text(), LARGE_MIXED_PAYLOAD);
    }

    /// Behavior: while a paste-like burst is active, Enter should not submit; it should insert a
    /// newline into the buffered payload and flush as a single paste later.
    #[test]
    fn ascii_burst_treats_enter_as_newline() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);

        let mut now = Instant::now();
        let step = Duration::from_millis(1);

        let _ = composer.handle_input_basic_with_time(
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            now,
        );
        now += step;
        let _ = composer.handle_input_basic_with_time(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            now,
        );
        now += step;

        let (result, _) = composer.handle_submission_with_time(!composer.steer_enabled, now);
        assert!(
            matches!(result, InputResult::None),
            "Enter during a burst should insert newline, not submit"
        );

        for ch in ['t', 'h', 'e', 'r', 'e'] {
            now += step;
            let _ = composer.handle_input_basic_with_time(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                now,
            );
        }

        assert!(composer.textarea.text().is_empty());
        let flush_time = now + PasteBurst::recommended_active_flush_delay() + step;
        let flushed = composer.handle_paste_burst_flush(flush_time);
        assert!(flushed, "expected paste burst to flush");
        assert_eq!(composer.textarea.text(), "hi\nthere");
    }

    /// Behavior: even if Enter suppression would normally be active for a burst, Enter should
    /// still dispatch a built-in slash command when the first line begins with `/`.
    #[test]
    fn slash_context_enter_ignores_paste_burst_enter_suppression() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::slash_command::SlashCommand;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        composer.textarea.set_text_clearing_elements("/diff");
        composer.textarea.set_cursor("/diff".len());
        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(result, InputResult::Command(SlashCommand::Diff)));
    }

    /// Behavior: if a burst is buffering text and the user presses a non-char key, flush the
    /// buffered burst *before* applying that key so the buffer cannot get stuck.
    #[test]
    fn non_char_key_flushes_active_burst_before_input() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        // Force an active burst so we can deterministically buffer characters without relying on
        // timing.
        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert!(composer.textarea.text().is_empty());
        assert!(composer.is_in_paste_burst());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(composer.textarea.text(), "hi");
        assert_eq!(composer.textarea.cursor(), 1);
        assert!(!composer.is_in_paste_burst());
    }

    /// Behavior: enabling `disable_paste_burst` flushes any held first character (flicker
    /// suppression) and then inserts subsequent chars immediately without creating burst state.
    #[test]
    fn disable_paste_burst_flushes_pending_first_char_and_inserts_immediately() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        // First ASCII char is normally held briefly. Flip the config mid-stream and ensure the
        // held char is not dropped.
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(composer.is_in_paste_burst());
        assert!(composer.textarea.text().is_empty());

        composer.set_disable_paste_burst(true);
        assert_eq!(composer.textarea.text(), "a");
        assert!(!composer.is_in_paste_burst());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(composer.textarea.text(), "ab");
        assert!(!composer.is_in_paste_burst());
    }

    /// Behavior: a small explicit paste inserts text directly (no placeholder), and the submitted
    /// text matches what is visible in the textarea.
    #[test]
    fn handle_paste_small_inserts_text() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);

        let needs_redraw = composer.handle_paste("hello".to_owned());
        assert!(needs_redraw);
        assert_eq!(composer.textarea.text(), "hello");
        assert!(composer.pending_pastes.is_empty());

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted { text, .. } => assert_eq!(text, "hello"),
            _ => panic!("expected Submitted"),
        }
    }

    #[test]
    fn empty_enter_returns_none() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);
        composer.set_steer_enabled(true);

        // Ensure composer is empty and press Enter.
        assert!(composer.textarea.text().is_empty());
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match result {
            InputResult::None => {}
            other => panic!("expected None for empty enter, got: {other:?}"),
        }
    }

    /// Behavior: a large explicit paste inserts a placeholder into the textarea, stores the full
    /// content in `pending_pastes`, and expands the placeholder to the full content on submit.
    #[test]
    fn handle_paste_large_uses_placeholder_and_replaces_on_submit() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        let large = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 10);
        let needs_redraw = composer.handle_paste(large.clone());
        assert!(needs_redraw);
        let placeholder = format!("[Pasted Content {} chars]", large.chars().count());
        assert_eq!(composer.textarea.text(), placeholder);
        assert_eq!(composer.pending_pastes.len(), 1);
        assert_eq!(composer.pending_pastes[0].0, placeholder);
        assert_eq!(composer.pending_pastes[0].1, large);

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted { text, .. } => assert_eq!(text, large),
            _ => panic!("expected Submitted"),
        }
        assert!(composer.pending_pastes.is_empty());
    }

    /// Behavior: editing that removes a paste placeholder should also clear the associated
    /// `pending_pastes` entry so it cannot be submitted accidentally.
    #[test]
    fn edit_clears_pending_paste() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let large = "y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1);
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.handle_paste(large);
        assert_eq!(composer.pending_pastes.len(), 1);

        // Any edit that removes the placeholder should clear pending_paste
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(composer.pending_pastes.is_empty());
    }

    #[test]
    fn ui_snapshots() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut terminal = match Terminal::new(TestBackend::new(100, 10)) {
            Ok(t) => t,
            Err(e) => panic!("Failed to create terminal: {e}"),
        };

        let test_cases = vec![
            ("empty", None),
            ("small", Some("short".to_owned())),
            ("large", Some("z".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5))),
            ("multiple_pastes", None),
            ("backspace_after_pastes", None),
        ];

        for (name, input) in test_cases {
            // Create a fresh composer for each test case
            let mut composer = ChatComposer::new(
                true,
                sender.clone(),
                false,
                "Ask Savfox to do anything".to_owned(),
                false,
            );

            if let Some(text) = input {
                composer.handle_paste(text);
            } else if name == "multiple_pastes" {
                // First large paste
                composer.handle_paste("x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 3));
                // Second large paste
                composer.handle_paste("y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 7));
                // Small paste
                composer.handle_paste(" another short paste".to_owned());
            } else if name == "backspace_after_pastes" {
                // Three large pastes
                composer.handle_paste("a".repeat(LARGE_PASTE_CHAR_THRESHOLD + 2));
                composer.handle_paste("b".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4));
                composer.handle_paste("c".repeat(LARGE_PASTE_CHAR_THRESHOLD + 6));
                // Move cursor to end and press backspace
                composer.textarea.set_cursor(composer.textarea.text().len());
                composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
            }

            terminal
                .draw(|f| composer.render(f.area(), f.buffer_mut()))
                .unwrap_or_else(|e| panic!("Failed to draw {name} composer: {e}"));

            insta::assert_snapshot!(name, terminal.backend());
        }
    }

    #[test]
    fn image_placeholder_snapshots() {
        snapshot_composer_state("image_placeholder_single", false, |composer| {
            composer.attach_image(PathBuf::from("/tmp/image1.png"));
        });

        snapshot_composer_state("image_placeholder_multiple", false, |composer| {
            composer.attach_image(PathBuf::from("/tmp/image1.png"));
            composer.attach_image(PathBuf::from("/tmp/image2.png"));
        });
    }

    #[test]
    fn slash_popup_model_first_for_mo_ui() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);

        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        // Type "/mo" humanlike so paste-burst doesn’t interfere.
        type_chars_humanlike(&mut composer, &['/', 'm', 'o']);

        let mut terminal = match Terminal::new(TestBackend::new(60, 5)) {
            Ok(t) => t,
            Err(e) => panic!("Failed to create terminal: {e}"),
        };
        terminal
            .draw(|f| composer.render(f.area(), f.buffer_mut()))
            .unwrap_or_else(|e| panic!("Failed to draw composer: {e}"));

        // Visual snapshot should show the slash popup with /model as the first entry.
        insta::assert_snapshot!("slash_popup_mo", terminal.backend());
    }

    #[test]
    fn slash_popup_model_first_for_mo_logic() {
        use super::super::command_popup::CommandItem;
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        type_chars_humanlike(&mut composer, &['/', 'm', 'o']);

        match &composer.active_popup {
            ActivePopup::Command(popup) => match popup.selected_item() {
                Some(CommandItem::Builtin(cmd)) => {
                    assert_eq!(cmd.command(), "model")
                }
                Some(CommandItem::UserPrompt(_)) => {
                    panic!("unexpected prompt selected for '/mo'")
                }
                None => panic!("no selected command for '/mo'"),
            },
            _ => panic!("slash popup not active after typing '/mo'"),
        }
    }

    #[test]
    fn slash_popup_resume_for_res_ui() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);

        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        // Type "/res" humanlike so paste-burst doesn’t interfere.
        type_chars_humanlike(&mut composer, &['/', 'r', 'e', 's']);

        let mut terminal = Terminal::new(TestBackend::new(60, 6)).expect("terminal");
        terminal
            .draw(|f| composer.render(f.area(), f.buffer_mut()))
            .expect("draw composer");

        // Snapshot should show /resume as the first entry for /res.
        insta::assert_snapshot!("slash_popup_res", terminal.backend());
    }

    #[test]
    fn slash_popup_resume_for_res_logic() {
        use super::super::command_popup::CommandItem;
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        type_chars_humanlike(&mut composer, &['/', 'r', 'e', 's']);

        match &composer.active_popup {
            ActivePopup::Command(popup) => match popup.selected_item() {
                Some(CommandItem::Builtin(cmd)) => {
                    assert_eq!(cmd.command(), "resume")
                }
                Some(CommandItem::UserPrompt(_)) => {
                    panic!("unexpected prompt selected for '/res'")
                }
                None => panic!("no selected command for '/res'"),
            },
            _ => panic!("slash popup not active after typing '/res'"),
        }
    }

    fn flush_after_paste_burst(composer: &mut ChatComposer) -> bool {
        std::thread::sleep(PasteBurst::recommended_active_flush_delay());
        composer.flush_paste_burst_if_due()
    }

    // Test helper: simulate human typing by advancing virtual time past the paste-burst window.
    fn type_chars_humanlike(composer: &mut ChatComposer, chars: &[char]) {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut now = Instant::now();
        let step = ChatComposer::recommended_paste_flush_delay();
        for &ch in chars {
            let _ = composer.handle_input_basic_with_time(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                now,
            );
            now += step;
            let _ = composer.handle_paste_burst_flush(now);
            composer.sync_popups();
        }
    }

    #[test]
    fn slash_init_dispatches_command_and_does_not_submit_literal_text() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        // Type the slash command.
        type_chars_humanlike(&mut composer, &['/', 'i', 'n', 'i', 't']);

        // Press Enter to dispatch the selected command.
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // When a slash command is dispatched, the composer should return a
        // Command result (not submit literal text) and clear its textarea.
        match result {
            InputResult::Command(cmd) => {
                assert_eq!(cmd.command(), "init");
            }
            InputResult::CommandWithArgs(..) => {
                panic!("expected command dispatch without args for '/init'")
            }
            InputResult::Submitted { text, .. } => {
                panic!("expected command dispatch, but composer submitted literal text: {text}")
            }
            InputResult::Queued { .. } => {
                panic!("expected command dispatch, but composer queued literal text")
            }
            InputResult::None => panic!("expected Command result for '/init'"),
        }
        assert!(composer.textarea.is_empty(), "composer should be cleared");
    }

    #[test]
    fn slash_command_disabled_while_task_running_keeps_text() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_task_running(true);
        composer
            .textarea
            .set_text_clearing_elements("/review these changes");

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(InputResult::None, result);
        assert_eq!("/review these changes", composer.textarea.text());

        let mut found_error = false;
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                let message = cell
                    .display_lines(80)
                    .into_iter()
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(message.contains("disabled while a task is in progress"));
                found_error = true;
                break;
            }
        }
        assert!(found_error, "expected error history cell to be sent");
    }

    #[test]
    fn extract_args_supports_quoted_paths_single_arg() {
        let args = extract_positional_args_for_prompt_line(
            "/prompts:review \"docs/My File.md\"",
            "review",
            &[],
        );
        assert_eq!(
            args,
            vec![PromptArg {
                text: "docs/My File.md".to_owned(),
                text_elements: Vec::new(),
            }]
        );
    }

    #[test]
    fn extract_args_supports_mixed_quoted_and_unquoted() {
        let args = extract_positional_args_for_prompt_line(
            "/prompts:cmd \"with spaces\" simple",
            "cmd",
            &[],
        );
        assert_eq!(
            args,
            vec![
                PromptArg {
                    text: "with spaces".to_owned(),
                    text_elements: Vec::new(),
                },
                PromptArg {
                    text: "simple".to_owned(),
                    text_elements: Vec::new(),
                }
            ]
        );
    }

    #[test]
    fn slash_tab_completion_moves_cursor_to_end() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        type_chars_humanlike(&mut composer, &['/', 'c']);

        let (_result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        let completed = composer.textarea.text();
        assert!(
            completed.starts_with("/c") && completed.ends_with(' '),
            "expected slash completion for /c, got: {completed:?}"
        );
        assert_eq!(composer.textarea.cursor(), completed.len());
    }

    #[test]
    fn slash_tab_then_enter_dispatches_builtin_command() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        // Type a prefix and complete with Tab, which inserts a trailing space
        // and moves the cursor beyond the '/name' token (hides the popup).
        type_chars_humanlike(&mut composer, &['/', 'd', 'i']);
        let (_res, _redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(composer.textarea.text(), "/diff ");

        // Press Enter: should dispatch the command, not submit literal text.
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Command(cmd) => assert_eq!(cmd.command(), "diff"),
            InputResult::CommandWithArgs(..) => {
                panic!("expected command dispatch without args for '/diff'")
            }
            InputResult::Submitted { text, .. } => {
                panic!("expected command dispatch after Tab completion, got literal submit: {text}")
            }
            InputResult::Queued { .. } => {
                panic!("expected command dispatch after Tab completion, got literal queue")
            }
            InputResult::None => panic!("expected Command result for '/diff'"),
        }
        assert!(composer.textarea.is_empty());
    }

    #[test]
    fn slash_command_elementizes_on_space() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_collaboration_modes_enabled(true);

        type_chars_humanlike(&mut composer, &['/', 'p', 'l', 'a', 'n', ' ']);

        let text = composer.textarea.text().to_owned();
        let elements = composer.textarea.text_elements();
        assert_eq!(text, "/plan ");
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].placeholder(&text), Some("/plan"));
    }

    #[test]
    fn slash_command_elementizes_only_known_commands() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_collaboration_modes_enabled(true);

        type_chars_humanlike(&mut composer, &['/', 'U', 's', 'e', 'r', 's', ' ']);

        let text = composer.textarea.text().to_owned();
        let elements = composer.textarea.text_elements();
        assert_eq!(text, "/Users ");
        assert!(elements.is_empty());
    }

    #[test]
    fn slash_command_element_removed_when_not_at_start() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        type_chars_humanlike(&mut composer, &['/', 'r', 'e', 'v', 'i', 'e', 'w', ' ']);

        let text = composer.textarea.text().to_owned();
        let elements = composer.textarea.text_elements();
        assert_eq!(text, "/review ");
        assert_eq!(elements.len(), 1);

        composer.textarea.set_cursor(0);
        type_chars_humanlike(&mut composer, &['x']);

        let text = composer.textarea.text().to_owned();
        let elements = composer.textarea.text_elements();
        assert_eq!(text, "x/review ");
        assert!(elements.is_empty());
    }

    #[test]
    fn slash_mention_dispatches_command_and_inserts_at() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        type_chars_humanlike(&mut composer, &['/', 'm', 'e', 'n', 't', 'i', 'o', 'n']);

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match result {
            InputResult::Command(cmd) => {
                assert_eq!(cmd.command(), "mention");
            }
            InputResult::CommandWithArgs(..) => {
                panic!("expected command dispatch without args for '/mention'")
            }
            InputResult::Submitted { text, .. } => {
                panic!("expected command dispatch, but composer submitted literal text: {text}")
            }
            InputResult::Queued { .. } => {
                panic!("expected command dispatch, but composer queued literal text")
            }
            InputResult::None => panic!("expected Command result for '/mention'"),
        }
        assert!(composer.textarea.is_empty(), "composer should be cleared");
        composer.insert_str("@");
        assert_eq!(composer.textarea.text(), "@");
    }

    #[test]
    fn slash_plan_args_preserve_text_elements() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_collaboration_modes_enabled(true);

        type_chars_humanlike(&mut composer, &['/', 'p', 'l', 'a', 'n', ' ']);
        let placeholder = local_image_label_text(1);
        composer.attach_image(PathBuf::from("/tmp/plan.png"));

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match result {
            InputResult::CommandWithArgs(cmd, args, text_elements) => {
                assert_eq!(cmd.command(), "plan");
                assert_eq!(args, placeholder);
                assert_eq!(text_elements.len(), 1);
                assert_eq!(
                    text_elements[0].placeholder(&args),
                    Some(placeholder.as_str())
                );
            }
            _ => panic!("expected CommandWithArgs for /plan with args"),
        }
    }

    /// Behavior: multiple paste operations can coexist; placeholders should be expanded to their
    /// original content on submission.
    #[test]
    fn test_multiple_pastes_submission() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        // Define test cases: (paste content, is_large)
        let test_cases = [
            ("x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 3), true),
            (" and ".to_owned(), false),
            ("y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 7), true),
        ];

        // Expected states after each paste
        let mut expected_text = String::new();
        let mut expected_pending_count = 0;

        // Apply all pastes and build expected state
        let states: Vec<_> = test_cases
            .iter()
            .map(|(content, is_large)| {
                composer.handle_paste(content.clone());
                if *is_large {
                    let placeholder = format!("[Pasted Content {} chars]", content.chars().count());
                    expected_text.push_str(&placeholder);
                    expected_pending_count += 1;
                } else {
                    expected_text.push_str(content);
                }
                (expected_text.clone(), expected_pending_count)
            })
            .collect();

        // Verify all intermediate states were correct
        assert_eq!(
            states,
            vec![
                (
                    format!("[Pasted Content {} chars]", test_cases[0].0.chars().count()),
                    1
                ),
                (
                    format!(
                        "[Pasted Content {} chars] and ",
                        test_cases[0].0.chars().count()
                    ),
                    1
                ),
                (
                    format!(
                        "[Pasted Content {} chars] and [Pasted Content {} chars]",
                        test_cases[0].0.chars().count(),
                        test_cases[2].0.chars().count()
                    ),
                    2
                ),
            ]
        );

        // Submit and verify final expansion
        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        if let InputResult::Submitted { text, .. } = result {
            assert_eq!(text, format!("{} and {}", test_cases[0].0, test_cases[2].0));
        } else {
            panic!("expected Submitted");
        }
    }

    #[test]
    fn test_placeholder_deletion() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        // Define test cases: (content, is_large)
        let test_cases = [
            ("a".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5), true),
            (" and ".to_owned(), false),
            ("b".repeat(LARGE_PASTE_CHAR_THRESHOLD + 6), true),
        ];

        // Apply all pastes
        let mut current_pos = 0;
        let states: Vec<_> = test_cases
            .iter()
            .map(|(content, is_large)| {
                composer.handle_paste(content.clone());
                if *is_large {
                    let placeholder = format!("[Pasted Content {} chars]", content.chars().count());
                    current_pos += placeholder.len();
                } else {
                    current_pos += content.len();
                }
                (
                    composer.textarea.text().to_owned(),
                    composer.pending_pastes.len(),
                    current_pos,
                )
            })
            .collect();

        // Delete placeholders one by one and collect states
        let mut deletion_states = vec![];

        // First deletion
        composer.textarea.set_cursor(states[0].2);
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        deletion_states.push((
            composer.textarea.text().to_owned(),
            composer.pending_pastes.len(),
        ));

        // Second deletion
        composer.textarea.set_cursor(composer.textarea.text().len());
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        deletion_states.push((
            composer.textarea.text().to_owned(),
            composer.pending_pastes.len(),
        ));

        // Verify all states
        assert_eq!(
            deletion_states,
            vec![
                (" and [Pasted Content 1006 chars]".to_owned(), 1),
                (" and ".to_owned(), 0),
            ]
        );
    }

    /// Behavior: if multiple large pastes share the same placeholder label (same char count),
    /// deleting one placeholder removes only its corresponding `pending_pastes` entry.
    #[test]
    fn deleting_duplicate_length_pastes_removes_only_target() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
        let placeholder_base = format!("[Pasted Content {} chars]", paste.chars().count());
        let placeholder_second = format!("{placeholder_base} #2");

        composer.handle_paste(paste.clone());
        composer.handle_paste(paste.clone());
        assert_eq!(
            composer.textarea.text(),
            format!("{placeholder_base}{placeholder_second}")
        );
        assert_eq!(composer.pending_pastes.len(), 2);

        composer.textarea.set_cursor(composer.textarea.text().len());
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        assert_eq!(composer.textarea.text(), placeholder_base);
        assert_eq!(composer.pending_pastes.len(), 1);
        assert_eq!(composer.pending_pastes[0].0, placeholder_base);
        assert_eq!(composer.pending_pastes[0].1, paste);
    }

    /// Behavior: large-paste placeholder numbering does not get reused after deletion, so a new
    /// paste of the same length gets a new unique placeholder label.
    #[test]
    fn large_paste_numbering_does_not_reuse_after_deletion() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
        let base = format!("[Pasted Content {} chars]", paste.chars().count());
        let second = format!("{base} #2");
        let third = format!("{base} #3");

        composer.handle_paste(paste.clone());
        composer.handle_paste(paste.clone());
        assert_eq!(composer.textarea.text(), format!("{base}{second}"));

        composer.textarea.set_cursor(base.len());
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.textarea.text(), second);
        assert_eq!(composer.pending_pastes.len(), 1);
        assert_eq!(composer.pending_pastes[0].0, second);

        composer.textarea.set_cursor(composer.textarea.text().len());
        composer.handle_paste(paste);

        assert_eq!(composer.textarea.text(), format!("{second}{third}"));
        assert_eq!(composer.pending_pastes.len(), 2);
        assert_eq!(composer.pending_pastes[0].0, second);
        assert_eq!(composer.pending_pastes[1].0, third);
    }

    #[test]
    fn test_partial_placeholder_deletion() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        // Define test cases: (cursor_position_from_end, expected_pending_count)
        let test_cases = [
            5, // Delete from middle - should clear tracking
            0, // Delete from end - should clear tracking
        ];

        let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
        let placeholder = format!("[Pasted Content {} chars]", paste.chars().count());

        let states: Vec<_> = test_cases
            .into_iter()
            .map(|pos_from_end| {
                composer.handle_paste(paste.clone());
                composer
                    .textarea
                    .set_cursor(placeholder.len() - pos_from_end);
                composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
                let result = (
                    composer.textarea.text().contains(&placeholder),
                    composer.pending_pastes.len(),
                );
                composer.textarea.set_text_clearing_elements("");
                result
            })
            .collect();

        assert_eq!(
            states,
            vec![
                (false, 0), // After deleting from middle
                (false, 0), // After deleting from end
            ]
        );
    }

    // --- Image attachment tests ---
    #[test]
    fn attach_image_and_submit_includes_local_image_paths() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        let path = PathBuf::from("/tmp/image1.png");
        composer.attach_image(path.clone());
        composer.handle_paste(" hi".into());
        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                assert_eq!(text, "[Image #1] hi");
                assert_eq!(text_elements.len(), 1);
                assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
                assert_eq!(
                    text_elements[0].byte_range,
                    ByteRange {
                        start: 0,
                        end: "[Image #1]".len()
                    }
                );
            }
            _ => panic!("expected Submitted"),
        }
        let imgs = composer.take_recent_submission_images();
        assert_eq!(vec![path], imgs);
    }

    #[test]
    fn history_navigation_restores_image_attachments() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        let path = PathBuf::from("/tmp/image1.png");
        composer.attach_image(path.clone());

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(result, InputResult::Submitted { .. }));

        composer.set_text_content(String::new(), Vec::new(), Vec::new());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

        let text = composer.current_text();
        assert_eq!(text, "[Image #1]");
        let text_elements = composer.text_elements();
        assert_eq!(text_elements.len(), 1);
        assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
        assert_eq!(composer.local_image_paths(), vec![path]);
    }

    #[test]
    fn set_text_content_reattaches_images_without_placeholder_metadata() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let placeholder = local_image_label_text(1);
        let text = format!("{placeholder} restored");
        let text_elements = vec![TextElement::new((0..placeholder.len()).into(), None)];
        let path = PathBuf::from("/tmp/image1.png");

        composer.set_text_content(text, text_elements, vec![path.clone()]);

        assert_eq!(composer.local_image_paths(), vec![path]);
    }

    #[test]
    fn large_paste_preserves_image_text_elements_on_submit() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        let large_content = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5);
        composer.handle_paste(large_content.clone());
        composer.handle_paste(" ".into());
        let path = PathBuf::from("/tmp/image_with_paste.png");
        composer.attach_image(path.clone());

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                let expected = format!("{large_content} [Image #1]");
                assert_eq!(text, expected);
                assert_eq!(text_elements.len(), 1);
                assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
                assert_eq!(
                    text_elements[0].byte_range,
                    ByteRange {
                        start: large_content.len() + 1,
                        end: large_content.len() + 1 + "[Image #1]".len(),
                    }
                );
            }
            _ => panic!("expected Submitted"),
        }
        let imgs = composer.take_recent_submission_images();
        assert_eq!(vec![path], imgs);
    }

    #[test]
    fn large_paste_with_leading_whitespace_trims_and_shifts_elements() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        let large_content = format!("  {}", "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5));
        composer.handle_paste(large_content.clone());
        composer.handle_paste(" ".into());
        let path = PathBuf::from("/tmp/image_with_trim.png");
        composer.attach_image(path.clone());

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                let trimmed = large_content.trim().to_owned();
                assert_eq!(text, format!("{trimmed} [Image #1]"));
                assert_eq!(text_elements.len(), 1);
                assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
                assert_eq!(
                    text_elements[0].byte_range,
                    ByteRange {
                        start: trimmed.len() + 1,
                        end: trimmed.len() + 1 + "[Image #1]".len(),
                    }
                );
            }
            _ => panic!("expected Submitted"),
        }
        let imgs = composer.take_recent_submission_images();
        assert_eq!(vec![path], imgs);
    }

    #[test]
    fn pasted_crlf_normalizes_newlines_for_elements() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        let pasted = "line1\r\nline2\r\n".to_owned();
        composer.handle_paste(pasted);
        composer.handle_paste(" ".into());
        let path = PathBuf::from("/tmp/image_crlf.png");
        composer.attach_image(path.clone());

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                assert_eq!(text, "line1\nline2\n [Image #1]");
                assert!(!text.contains('\r'));
                assert_eq!(text_elements.len(), 1);
                assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
                assert_eq!(
                    text_elements[0].byte_range,
                    ByteRange {
                        start: "line1\nline2\n ".len(),
                        end: "line1\nline2\n [Image #1]".len(),
                    }
                );
            }
            _ => panic!("expected Submitted"),
        }
        let imgs = composer.take_recent_submission_images();
        assert_eq!(vec![path], imgs);
    }

    #[test]
    fn suppressed_submission_restores_pending_paste_payload() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.textarea.set_text_clearing_elements("/unknown ");
        composer.textarea.set_cursor("/unknown ".len());
        let large_content = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5);
        composer.handle_paste(large_content.clone());
        let placeholder = composer
            .pending_pastes
            .first()
            .expect("expected pending paste")
            .0
            .clone();

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(result, InputResult::None));
        assert_eq!(composer.pending_pastes.len(), 1);
        assert_eq!(composer.textarea.text(), format!("/unknown {placeholder}"));

        composer.textarea.set_cursor(0);
        composer.textarea.insert_str(" ");
        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                assert_eq!(text, format!("/unknown {large_content}"));
                assert!(text_elements.is_empty());
            }
            _ => panic!("expected Submitted"),
        }
        assert!(composer.pending_pastes.is_empty());
    }

    #[test]
    fn attach_image_without_text_submits_empty_text_and_images() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);
        let path = PathBuf::from("/tmp/image2.png");
        composer.attach_image(path.clone());
        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                assert_eq!(text, "[Image #1]");
                assert_eq!(text_elements.len(), 1);
                assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
                assert_eq!(
                    text_elements[0].byte_range,
                    ByteRange {
                        start: 0,
                        end: "[Image #1]".len()
                    }
                );
            }
            _ => panic!("expected Submitted"),
        }
        let imgs = composer.take_recent_submission_images();
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0], path);
        assert!(composer.attached_images.is_empty());
    }

    #[test]
    fn duplicate_image_placeholders_get_suffix() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        let path = PathBuf::from("/tmp/image_dup.png");
        composer.attach_image(path.clone());
        composer.handle_paste(" ".into());
        composer.attach_image(path);

        let text = composer.textarea.text().to_owned();
        assert!(text.contains("[Image #1]"));
        assert!(text.contains("[Image #2]"));
        assert_eq!(composer.attached_images[0].placeholder, "[Image #1]");
        assert_eq!(composer.attached_images[1].placeholder, "[Image #2]");
    }

    #[test]
    fn image_placeholder_backspace_behaves_like_text_placeholder() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        let path = PathBuf::from("/tmp/image3.png");
        composer.attach_image(path.clone());
        let placeholder = composer.attached_images[0].placeholder.clone();

        // Case 1: backspace at end
        composer.textarea.move_cursor_to_end_of_line(false);
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(!composer.textarea.text().contains(&placeholder));
        assert!(composer.attached_images.is_empty());

        // Re-add and ensure backspace at element start does not delete the placeholder.
        composer.attach_image(path);
        let placeholder2 = composer.attached_images[0].placeholder.clone();
        // Move cursor to roughly middle of placeholder
        if let Some(start_pos) = composer.textarea.text().find(&placeholder2) {
            let mid_pos = start_pos + (placeholder2.len() / 2);
            composer.textarea.set_cursor(mid_pos);
            composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
            assert!(composer.textarea.text().contains(&placeholder2));
            assert_eq!(composer.attached_images.len(), 1);
        } else {
            panic!("Placeholder not found in textarea");
        }
    }

    #[test]
    fn backspace_with_multibyte_text_before_placeholder_does_not_panic() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        // Insert an image placeholder at the start
        let path = PathBuf::from("/tmp/image_multibyte.png");
        composer.attach_image(path);
        // Add multibyte text after the placeholder
        composer.textarea.insert_str("日本語");

        // Cursor is at end; pressing backspace should delete the last character
        // without panicking and leave the placeholder intact.
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        assert_eq!(composer.attached_images.len(), 1);
        assert!(composer.textarea.text().starts_with("[Image #1]"));
    }

    #[test]
    fn deleting_one_of_duplicate_image_placeholders_removes_one_entry() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let path1 = PathBuf::from("/tmp/image_dup1.png");
        let path2 = PathBuf::from("/tmp/image_dup2.png");

        composer.attach_image(path1);
        // separate placeholders with a space for clarity
        composer.handle_paste(" ".into());
        composer.attach_image(path2.clone());

        let placeholder1 = composer.attached_images[0].placeholder.clone();
        let placeholder2 = composer.attached_images[1].placeholder.clone();
        let text = composer.textarea.text().to_owned();
        let start1 = text.find(&placeholder1).expect("first placeholder present");
        let end1 = start1 + placeholder1.len();
        composer.textarea.set_cursor(end1);

        // Backspace should delete the first placeholder and its mapping.
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        let new_text = composer.textarea.text().to_owned();
        assert_eq!(
            1,
            new_text.matches(&placeholder1).count(),
            "one placeholder remains after deletion"
        );
        assert_eq!(
            0,
            new_text.matches(&placeholder2).count(),
            "second placeholder was relabeled"
        );
        assert_eq!(
            1,
            new_text.matches("[Image #1]").count(),
            "remaining placeholder relabeled to #1"
        );
        assert_eq!(
            vec![AttachedImage {
                path: path2,
                placeholder: "[Image #1]".to_owned()
            }],
            composer.attached_images,
            "one image mapping remains"
        );
    }

    #[test]
    fn deleting_reordered_image_one_renumbers_text_in_place() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let path1 = PathBuf::from("/tmp/image_first.png");
        let path2 = PathBuf::from("/tmp/image_second.png");
        let placeholder1 = local_image_label_text(1);
        let placeholder2 = local_image_label_text(2);

        // Placeholders can be reordered in the text buffer; deleting image #1 should renumber
        // image #2 wherever it appears, not just after the cursor.
        let text = format!("Test {placeholder2} test {placeholder1}");
        let start2 = text.find(&placeholder2).expect("placeholder2 present");
        let start1 = text.find(&placeholder1).expect("placeholder1 present");
        let text_elements = vec![
            TextElement::new(
                ByteRange {
                    start: start2,
                    end: start2 + placeholder2.len(),
                },
                Some(placeholder2),
            ),
            TextElement::new(
                ByteRange {
                    start: start1,
                    end: start1 + placeholder1.len(),
                },
                Some(placeholder1.clone()),
            ),
        ];
        composer.set_text_content(text, text_elements, vec![path1, path2.clone()]);

        let end1 = start1 + placeholder1.len();
        composer.textarea.set_cursor(end1);

        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        assert_eq!(
            composer.textarea.text(),
            format!("Test {placeholder1} test ")
        );
        assert_eq!(
            vec![AttachedImage {
                path: path2,
                placeholder: placeholder1
            }],
            composer.attached_images,
            "attachment renumbered after deletion"
        );
    }

    #[test]
    fn deleting_first_text_element_renumbers_following_text_element() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let path1 = PathBuf::from("/tmp/image_first.png");
        let path2 = PathBuf::from("/tmp/image_second.png");

        // Insert two adjacent atomic elements.
        composer.attach_image(path1);
        composer.attach_image(path2.clone());
        assert_eq!(composer.textarea.text(), "[Image #1][Image #2]");
        assert_eq!(composer.attached_images.len(), 2);

        // Delete the first element using normal textarea editing (forward Delete at cursor start).
        composer.textarea.set_cursor(0);
        composer.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));

        // Remaining image should be renumbered and the textarea element updated.
        assert_eq!(composer.attached_images.len(), 1);
        assert_eq!(composer.attached_images[0].path, path2);
        assert_eq!(composer.attached_images[0].placeholder, "[Image #1]");
        assert_eq!(composer.textarea.text(), "[Image #1]");
    }

    #[test]
    fn pasting_filepath_attaches_image() {
        let tmp = tempdir().expect("create TempDir");
        let tmp_path: PathBuf = tmp.path().join("savfox_tui_test_paste_image.png");
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_fn(3, 2, |_x, _y| Rgba([1, 2, 3, 255]));
        img.save(&tmp_path).expect("failed to write temp png");

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let needs_redraw = composer.handle_paste(tmp_path.to_string_lossy().to_string());
        assert!(needs_redraw);
        assert!(composer.textarea.text().starts_with("[Image #1] "));

        let imgs = composer.take_recent_submission_images();
        assert_eq!(imgs, vec![tmp_path]);
    }

    #[test]
    fn selecting_custom_prompt_without_args_submits_content() {
        let prompt_text = "Hello from saved prompt";

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        // Inject prompts as if received via event.
        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: prompt_text.to_owned(),
            description: None,
            argument_hint: None,
        }]);

        type_chars_humanlike(
            &mut composer,
            &[
                '/', 'p', 'r', 'o', 'm', 'p', 't', 's', ':', 'm', 'y', '-', 'p', 'r', 'o', 'm',
                'p', 't',
            ],
        );

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            result,
            InputResult::Submitted { text, .. } if text == prompt_text
        ));
        assert!(composer.textarea.is_empty());
    }

    #[test]
    fn custom_prompt_submission_expands_arguments() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Review $USER changes on $BRANCH".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        composer
            .textarea
            .set_text_clearing_elements("/prompts:my-prompt USER=Alice BRANCH=main");

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            result,
            InputResult::Submitted { text, .. }
                if text == "Review Alice changes on main"
        ));
        assert!(composer.textarea.is_empty());
    }

    #[test]
    fn custom_prompt_submission_accepts_quoted_values() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Pair $USER with $BRANCH".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        composer
            .textarea
            .set_text_clearing_elements("/prompts:my-prompt USER=\"Alice Smith\" BRANCH=dev-main");

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            result,
            InputResult::Submitted { text, .. }
                if text == "Pair Alice Smith with dev-main"
        ));
        assert!(composer.textarea.is_empty());
    }

    #[test]
    fn custom_prompt_submission_preserves_image_placeholder_unquoted() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Review $IMG".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        composer
            .textarea
            .set_text_clearing_elements("/prompts:my-prompt IMG=");
        composer.textarea.set_cursor(composer.textarea.text().len());
        let path = PathBuf::from("/tmp/image_prompt.png");
        composer.attach_image(path);

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                let placeholder = local_image_label_text(1);
                assert_eq!(text, format!("Review {placeholder}"));
                assert_eq!(
                    text_elements,
                    vec![TextElement::new(
                        ByteRange {
                            start: "Review ".len(),
                            end: "Review ".len() + placeholder.len(),
                        },
                        Some(placeholder),
                    )]
                );
            }
            _ => panic!("expected Submitted"),
        }
    }

    #[test]
    fn custom_prompt_submission_preserves_image_placeholder_quoted() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Review $IMG".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        composer
            .textarea
            .set_text_clearing_elements("/prompts:my-prompt IMG=\"");
        composer.textarea.set_cursor(composer.textarea.text().len());
        let path = PathBuf::from("/tmp/image_prompt_quoted.png");
        composer.attach_image(path);
        composer.handle_paste("\"".to_owned());

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                let placeholder = local_image_label_text(1);
                assert_eq!(text, format!("Review {placeholder}"));
                assert_eq!(
                    text_elements,
                    vec![TextElement::new(
                        ByteRange {
                            start: "Review ".len(),
                            end: "Review ".len() + placeholder.len(),
                        },
                        Some(placeholder),
                    )]
                );
            }
            _ => panic!("expected Submitted"),
        }
    }

    #[test]
    fn custom_prompt_submission_drops_unused_image_arg() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Review changes".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        composer
            .textarea
            .set_text_clearing_elements("/prompts:my-prompt IMG=");
        composer.textarea.set_cursor(composer.textarea.text().len());
        let path = PathBuf::from("/tmp/unused_image.png");
        composer.attach_image(path);

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                assert_eq!(text, "Review changes");
                assert!(text_elements.is_empty());
            }
            _ => panic!("expected Submitted"),
        }
        assert!(composer.take_recent_submission_images().is_empty());
    }

    /// Behavior: selecting a custom prompt that includes a large paste placeholder should expand
    /// to the full pasted content before submission.
    #[test]
    fn custom_prompt_with_large_paste_expands_correctly() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        // Create a custom prompt with positional args (no named args like $USER)
        composer.set_custom_prompts(vec![CustomPrompt {
            name: "code-review".to_owned(),
            path: "/tmp/code-review.md".to_owned().into(),
            content: "Please review the following code:\n\n$1".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        // Type the slash command
        let command_text = "/prompts:code-review ";
        composer.textarea.set_text_clearing_elements(command_text);
        composer.textarea.set_cursor(command_text.len());

        // Paste large content (>3000 chars) to trigger placeholder
        let large_content = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 3000);
        composer.handle_paste(large_content.clone());

        // Verify placeholder was created
        let placeholder = format!("[Pasted Content {} chars]", large_content.chars().count());
        assert_eq!(
            composer.textarea.text(),
            format!("/prompts:code-review {}", placeholder)
        );
        assert_eq!(composer.pending_pastes.len(), 1);
        assert_eq!(composer.pending_pastes[0].0, placeholder);
        assert_eq!(composer.pending_pastes[0].1, large_content);

        // Submit by pressing Enter
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // Verify the custom prompt was expanded with the large content as positional arg
        match result {
            InputResult::Submitted { text, .. } => {
                // The prompt should be expanded, with the large content replacing $1
                assert_eq!(
                    text,
                    format!("Please review the following code:\n\n{}", large_content),
                    "Expected prompt expansion with large content as $1"
                );
            }
            _ => panic!("expected Submitted, got: {result:?}"),
        }
        assert!(composer.textarea.is_empty());
        assert!(composer.pending_pastes.is_empty());
    }

    #[test]
    fn custom_prompt_with_large_paste_and_image_preserves_elements() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Review $IMG\n\n$CODE".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        composer
            .textarea
            .set_text_clearing_elements("/prompts:my-prompt IMG=");
        composer.textarea.set_cursor(composer.textarea.text().len());
        let path = PathBuf::from("/tmp/image_prompt_combo.png");
        composer.attach_image(path);
        composer.handle_paste(" CODE=".to_owned());
        let large_content = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5);
        composer.handle_paste(large_content.clone());

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                let placeholder = local_image_label_text(1);
                assert_eq!(text, format!("Review {placeholder}\n\n{large_content}"));
                assert_eq!(
                    text_elements,
                    vec![TextElement::new(
                        ByteRange {
                            start: "Review ".len(),
                            end: "Review ".len() + placeholder.len(),
                        },
                        Some(placeholder),
                    )]
                );
            }
            _ => panic!("expected Submitted"),
        }
    }

    #[test]
    fn slash_path_input_submits_without_command_error() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer
            .textarea
            .set_text_clearing_elements("/Users/example/project/src/main.rs");

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        if let InputResult::Submitted { text, .. } = result {
            assert_eq!(text, "/Users/example/project/src/main.rs");
        } else {
            panic!("expected Submitted");
        }
        assert!(composer.textarea.is_empty());
        match rx.try_recv() {
            Ok(event) => panic!("unexpected event: {event:?}"),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            Err(err) => panic!("unexpected channel state: {err:?}"),
        }
    }

    #[test]
    fn slash_with_leading_space_submits_as_text() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer
            .textarea
            .set_text_clearing_elements(" /this-looks-like-a-command");

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        if let InputResult::Submitted { text, .. } = result {
            assert_eq!(text, "/this-looks-like-a-command");
        } else {
            panic!("expected Submitted");
        }
        assert!(composer.textarea.is_empty());
        match rx.try_recv() {
            Ok(event) => panic!("unexpected event: {event:?}"),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            Err(err) => panic!("unexpected channel state: {err:?}"),
        }
    }

    #[test]
    fn custom_prompt_invalid_args_reports_error() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Review $USER changes".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        composer
            .textarea
            .set_text_clearing_elements("/prompts:my-prompt USER=Alice stray");

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(InputResult::None, result);
        assert_eq!(
            "/prompts:my-prompt USER=Alice stray",
            composer.textarea.text()
        );

        let mut found_error = false;
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                let message = cell
                    .display_lines(80)
                    .into_iter()
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(message.contains("expected key=value"));
                found_error = true;
                break;
            }
        }
        assert!(found_error, "expected error history cell to be sent");
    }

    #[test]
    fn custom_prompt_missing_required_args_reports_error() {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Review $USER changes on $BRANCH".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        // Provide only one of the required args
        composer
            .textarea
            .set_text_clearing_elements("/prompts:my-prompt USER=Alice");

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(InputResult::None, result);
        assert_eq!("/prompts:my-prompt USER=Alice", composer.textarea.text());

        let mut found_error = false;
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                let message = cell
                    .display_lines(80)
                    .into_iter()
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(message.to_lowercase().contains("missing required args"));
                assert!(message.contains("BRANCH"));
                found_error = true;
                break;
            }
        }
        assert!(
            found_error,
            "expected missing args error history cell to be sent"
        );
    }

    #[test]
    fn selecting_custom_prompt_with_args_expands_placeholders() {
        // Support $1..$9 and $ARGUMENTS in prompt content.
        let prompt_text = "Header: $1\nArgs: $ARGUMENTS\nNinth: $9\n";

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: prompt_text.to_owned(),
            description: None,
            argument_hint: None,
        }]);

        // Type the slash command with two args and hit Enter to submit.
        type_chars_humanlike(
            &mut composer,
            &[
                '/', 'p', 'r', 'o', 'm', 'p', 't', 's', ':', 'm', 'y', '-', 'p', 'r', 'o', 'm',
                'p', 't', ' ', 'f', 'o', 'o', ' ', 'b', 'a', 'r',
            ],
        );
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let expected = "Header: foo\nArgs: foo bar\nNinth: \n".to_owned();
        assert!(matches!(
            result,
            InputResult::Submitted { text, .. } if text == expected
        ));
    }

    #[test]
    fn popup_prompt_submission_prunes_unused_image_attachments() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Hello".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        composer.attach_image(PathBuf::from("/tmp/unused.png"));
        composer.textarea.set_cursor(0);
        composer.handle_paste(format!("/{PROMPTS_CMD_PREFIX}:my-prompt "));

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            result,
            InputResult::Submitted { text, .. } if text == "Hello"
        ));
        assert!(
            composer
                .take_recent_submission_images_with_placeholders()
                .is_empty()
        );
    }

    #[test]
    fn numeric_prompt_auto_submit_prunes_unused_image_attachments() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Hello $1".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        type_chars_humanlike(
            &mut composer,
            &[
                '/', 'p', 'r', 'o', 'm', 'p', 't', 's', ':', 'm', 'y', '-', 'p', 'r', 'o', 'm',
                'p', 't', ' ', 'f', 'o', 'o', ' ',
            ],
        );
        composer.attach_image(PathBuf::from("/tmp/unused.png"));

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            result,
            InputResult::Submitted { text, .. } if text == "Hello foo"
        ));
        assert!(
            composer
                .take_recent_submission_images_with_placeholders()
                .is_empty()
        );
    }

    #[test]
    fn numeric_prompt_auto_submit_expands_pending_pastes() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Echo: $1".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        composer
            .textarea
            .set_text_clearing_elements("/prompts:my-prompt ");
        composer.textarea.set_cursor(composer.textarea.text().len());
        let large_content = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5);
        composer.handle_paste(large_content.clone());

        assert_eq!(composer.pending_pastes.len(), 1);

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let expected = format!("Echo: {large_content}");
        assert!(matches!(
            result,
            InputResult::Submitted { text, .. } if text == expected
        ));
        assert!(composer.pending_pastes.is_empty());
    }

    #[test]
    fn queued_prompt_submission_prunes_unused_image_attachments() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(false);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: "Hello $1".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        composer
            .textarea
            .set_text_clearing_elements("/prompts:my-prompt foo ");
        composer.textarea.set_cursor(composer.textarea.text().len());
        composer.attach_image(PathBuf::from("/tmp/unused.png"));

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            result,
            InputResult::Queued { text, .. } if text == "Hello foo"
        ));
        assert!(
            composer
                .take_recent_submission_images_with_placeholders()
                .is_empty()
        );
    }

    #[test]
    fn selecting_custom_prompt_with_positional_args_submits_numeric_expansion() {
        let prompt_text = "Header: $1\nArgs: $ARGUMENTS\n";

        let prompt = CustomPrompt {
            name: "my-prompt".to_owned(),
            path: "/tmp/my-prompt.md".to_owned().into(),
            content: prompt_text.to_owned(),
            description: None,
            argument_hint: None,
        };

        let action = prompt_selection_action(
            &prompt,
            "/prompts:my-prompt foo bar",
            PromptSelectionMode::Submit,
            &[],
        );
        match action {
            PromptSelectionAction::Submit {
                text,
                text_elements,
            } => {
                assert_eq!(text, "Header: foo\nArgs: foo bar\n");
                assert!(text_elements.is_empty());
            }
            _ => panic!("expected Submit action"),
        }
    }

    #[test]
    fn numeric_prompt_positional_args_does_not_error() {
        // Ensure that a prompt with only numeric placeholders does not trigger
        // key=value parsing errors when given positional arguments.
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "elegant".to_owned(),
            path: "/tmp/elegant.md".to_owned().into(),
            content: "Echo: $ARGUMENTS".to_owned(),
            description: None,
            argument_hint: None,
        }]);

        // Type positional args; should submit with numeric expansion, no errors.
        composer
            .textarea
            .set_text_clearing_elements("/prompts:elegant hi");
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            result,
            InputResult::Submitted { text, .. } if text == "Echo: hi"
        ));
        assert!(composer.textarea.is_empty());
    }

    #[test]
    fn selecting_custom_prompt_with_no_args_inserts_template() {
        let prompt_text = "X:$1 Y:$2 All:[$ARGUMENTS]";

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "p".to_owned(),
            path: "/tmp/p.md".to_owned().into(),
            content: prompt_text.to_owned(),
            description: None,
            argument_hint: None,
        }]);

        type_chars_humanlike(
            &mut composer,
            &['/', 'p', 'r', 'o', 'm', 'p', 't', 's', ':', 'p'],
        );
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        // With no args typed, selecting the prompt inserts the command template
        // and does not submit immediately.
        assert_eq!(InputResult::None, result);
        assert_eq!("/prompts:p ", composer.textarea.text());
    }

    #[test]
    fn selecting_custom_prompt_preserves_literal_dollar_dollar() {
        // '$$' should remain untouched.
        let prompt_text = "Cost: $$ and first: $1";

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "price".to_owned(),
            path: "/tmp/price.md".to_owned().into(),
            content: prompt_text.to_owned(),
            description: None,
            argument_hint: None,
        }]);

        type_chars_humanlike(
            &mut composer,
            &[
                '/', 'p', 'r', 'o', 'm', 'p', 't', 's', ':', 'p', 'r', 'i', 'c', 'e', ' ', 'x',
            ],
        );
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            result,
            InputResult::Submitted { text, .. }
                if text == "Cost: $$ and first: x"
        ));
    }

    #[test]
    fn selecting_custom_prompt_reuses_cached_arguments_join() {
        let prompt_text = "First: $ARGUMENTS\nSecond: $ARGUMENTS";

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );
        composer.set_steer_enabled(true);

        composer.set_custom_prompts(vec![CustomPrompt {
            name: "repeat".to_owned(),
            path: "/tmp/repeat.md".to_owned().into(),
            content: prompt_text.to_owned(),
            description: None,
            argument_hint: None,
        }]);

        type_chars_humanlike(
            &mut composer,
            &[
                '/', 'p', 'r', 'o', 'm', 'p', 't', 's', ':', 'r', 'e', 'p', 'e', 'a', 't', ' ',
                'o', 'n', 'e', ' ', 't', 'w', 'o',
            ],
        );
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let expected = "First: one two\nSecond: one two".to_owned();
        assert!(matches!(
            result,
            InputResult::Submitted { text, .. } if text == expected
        ));
    }

    /// Behavior: the first fast ASCII character is held briefly to avoid flicker; if no burst
    /// follows, it should eventually flush as normal typed input (not as a paste).
    #[test]
    fn pending_first_ascii_char_flushes_as_typed() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(composer.is_in_paste_burst());
        assert!(composer.textarea.text().is_empty());

        std::thread::sleep(ChatComposer::recommended_paste_flush_delay());
        let flushed = composer.flush_paste_burst_if_due();
        assert!(flushed, "expected pending first char to flush");
        assert_eq!(composer.textarea.text(), "h");
        assert!(!composer.is_in_paste_burst());
    }

    /// Behavior: fast "paste-like" ASCII input should buffer and then flush as a single paste. If
    /// the payload is small, it should insert directly (no placeholder).
    #[test]
    fn burst_paste_fast_small_buffers_and_flushes_on_stop() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let count = 32;
        let mut now = Instant::now();
        let step = Duration::from_millis(1);
        for _ in 0..count {
            let _ = composer.handle_input_basic_with_time(
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
                now,
            );
            assert!(
                composer.is_in_paste_burst(),
                "expected active paste burst during fast typing"
            );
            assert!(
                composer.textarea.text().is_empty(),
                "text should not appear during burst"
            );
            now += step;
        }

        assert!(
            composer.textarea.text().is_empty(),
            "text should remain empty until flush"
        );
        let flush_time = now + PasteBurst::recommended_active_flush_delay() + step;
        let flushed = composer.handle_paste_burst_flush(flush_time);
        assert!(flushed, "expected buffered text to flush after stop");
        assert_eq!(composer.textarea.text(), "a".repeat(count));
        assert!(
            composer.pending_pastes.is_empty(),
            "no placeholder for small burst"
        );
    }

    /// Behavior: fast "paste-like" ASCII input should buffer and then flush as a single paste. If
    /// the payload is large, it should insert a placeholder and defer the full text until submit.
    #[test]
    fn burst_paste_fast_large_inserts_placeholder_on_flush() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let count = LARGE_PASTE_CHAR_THRESHOLD + 1; // > threshold to trigger placeholder
        let mut now = Instant::now();
        let step = Duration::from_millis(1);
        for _ in 0..count {
            let _ = composer.handle_input_basic_with_time(
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
                now,
            );
            now += step;
        }

        // Nothing should appear until we stop and flush
        assert!(composer.textarea.text().is_empty());
        let flush_time = now + PasteBurst::recommended_active_flush_delay() + step;
        let flushed = composer.handle_paste_burst_flush(flush_time);
        assert!(flushed, "expected flush after stopping fast input");

        let expected_placeholder = format!("[Pasted Content {count} chars]");
        assert_eq!(composer.textarea.text(), expected_placeholder);
        assert_eq!(composer.pending_pastes.len(), 1);
        assert_eq!(composer.pending_pastes[0].0, expected_placeholder);
        assert_eq!(composer.pending_pastes[0].1.len(), count);
        assert!(composer.pending_pastes[0].1.chars().all(|c| c == 'x'));
    }

    /// Behavior: human-like typing (with delays between chars) should not be classified as a paste
    /// burst. Characters should appear immediately and should not trigger a paste placeholder.
    #[test]
    fn humanlike_typing_1000_chars_appears_live_no_placeholder() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let count = LARGE_PASTE_CHAR_THRESHOLD; // 1000 in current config
        let chars: Vec<char> = vec!['z'; count];
        type_chars_humanlike(&mut composer, &chars);

        assert_eq!(composer.textarea.text(), "z".repeat(count));
        assert!(composer.pending_pastes.is_empty());
    }

    #[test]
    fn slash_popup_not_activated_for_slash_space_text_history_like_input() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio::sync::mpsc::unbounded_channel;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        // Simulate history-like content: "/ test"
        composer.set_text_content("/ test".to_owned(), Vec::new(), Vec::new());

        // After set_text_content -> sync_popups is called; popup should NOT be Command.
        assert!(
            matches!(composer.active_popup, ActivePopup::None),
            "expected no slash popup for '/ test'"
        );

        // Up should be handled by history navigation path, not slash popup handler.
        let (result, _redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(result, InputResult::None);
    }

    #[test]
    fn slash_popup_activated_for_bare_slash_and_valid_prefixes() {
        // use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use tokio::sync::mpsc::unbounded_channel;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        // Case 1: bare "/"
        composer.set_text_content("/".to_owned(), Vec::new(), Vec::new());
        assert!(
            matches!(composer.active_popup, ActivePopup::Command(_)),
            "bare '/' should activate slash popup"
        );

        // Case 2: valid prefix "/re" (matches /review, /resume, etc.)
        composer.set_text_content("/re".to_owned(), Vec::new(), Vec::new());
        assert!(
            matches!(composer.active_popup, ActivePopup::Command(_)),
            "'/re' should activate slash popup via prefix match"
        );

        // Case 3: fuzzy match "/ac" (subsequence of /compact and /feedback)
        composer.set_text_content("/ac".to_owned(), Vec::new(), Vec::new());
        assert!(
            matches!(composer.active_popup, ActivePopup::Command(_)),
            "'/ac' should activate slash popup via fuzzy match"
        );

        // Case 4: invalid prefix "/zzz" – still allowed to open popup if it
        // matches no built-in command; our current logic will not open popup.
        // Verify that explicitly.
        composer.set_text_content("/zzz".to_owned(), Vec::new(), Vec::new());
        assert!(
            matches!(composer.active_popup, ActivePopup::None),
            "'/zzz' should not activate slash popup because it is not a prefix of any built-in command"
        );
    }

    #[test]
    fn apply_external_edit_rebuilds_text_and_attachments() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let placeholder = local_image_label_text(1);
        composer.textarea.insert_element(&placeholder);
        composer.attached_images.push(AttachedImage {
            placeholder: placeholder.clone(),
            path: PathBuf::from("img.png"),
        });
        composer
            .pending_pastes
            .push(("[Pasted]".to_owned(), "data".to_owned()));

        composer.apply_external_edit(format!("Edited {placeholder} text"));

        assert_eq!(
            composer.current_text(),
            format!("Edited {placeholder} text")
        );
        assert!(composer.pending_pastes.is_empty());
        assert_eq!(composer.attached_images.len(), 1);
        assert_eq!(composer.attached_images[0].placeholder, placeholder);
        assert_eq!(composer.textarea.cursor(), composer.current_text().len());
    }

    #[test]
    fn apply_external_edit_drops_missing_attachments() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let placeholder = local_image_label_text(1);
        composer.textarea.insert_element(&placeholder);
        composer.attached_images.push(AttachedImage {
            placeholder: placeholder.clone(),
            path: PathBuf::from("img.png"),
        });

        composer.apply_external_edit("No images here".to_owned());

        assert_eq!(composer.current_text(), "No images here".to_owned());
        assert!(composer.attached_images.is_empty());
    }

    #[test]
    fn current_text_with_pending_expands_placeholders() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let placeholder = "[Pasted Content 5 chars]".to_owned();
        composer.textarea.insert_element(&placeholder);
        composer
            .pending_pastes
            .push((placeholder.clone(), "hello".to_owned()));

        assert_eq!(
            composer.current_text_with_pending(),
            "hello".to_owned(),
            "placeholder should expand to actual text"
        );
    }

    #[test]
    fn apply_external_edit_limits_duplicates_to_occurrences() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        let placeholder = local_image_label_text(1);
        composer.textarea.insert_element(&placeholder);
        composer.attached_images.push(AttachedImage {
            placeholder: placeholder.clone(),
            path: PathBuf::from("img.png"),
        });

        composer.apply_external_edit(format!("{placeholder} extra {placeholder}"));

        assert_eq!(
            composer.current_text(),
            format!("{placeholder} extra {placeholder}")
        );
        assert_eq!(composer.attached_images.len(), 1);
    }

    #[test]
    fn input_disabled_ignores_keypresses_and_hides_cursor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Ask Savfox to do anything".to_owned(),
            false,
        );

        composer.set_text_content("hello".to_owned(), Vec::new(), Vec::new());
        composer.set_input_enabled(false, Some("Input disabled for test.".to_owned()));

        let (result, needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        assert_eq!(result, InputResult::None);
        assert!(!needs_redraw);
        assert_eq!(composer.current_text(), "hello");

        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 5,
        };
        assert_eq!(composer.cursor_pos(area), None);
    }
