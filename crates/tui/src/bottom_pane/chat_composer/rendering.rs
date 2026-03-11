//! Rendering logic for the ChatComposer widget.
//!
//! This module contains the `Renderable` trait implementation and all layout
//! and rendering helpers for the composer, including footer rendering in both
//! session mode and startup mode.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, StatefulWidgetRef, WidgetRef};

use super::{ActivePopup, ChatComposer, FOOTER_SPACING_HEIGHT, STARTUP_PLACEHOLDER_TEXT};
use crate::bottom_pane::footer::{
    FooterMode, FooterProps, SummaryLeft, can_show_left_with_context, context_window_line,
    footer_height, footer_hint_items_width, footer_line_width, inset_footer_hint_area,
    render_context_right, render_footer_from_props, render_footer_hint_items, render_footer_line,
    render_model_status_line, render_path_info_line, render_shortcut_hints_line,
    single_line_footer_layout,
};
use crate::render::renderable::Renderable;
use crate::render::{Insets, RectExt};
use crate::style::{cursor_fg, input_bg, user_message_style};
use crate::ui_consts::LIVE_PREFIX_COLS;

impl ChatComposer {
    pub(super) fn layout_areas(&self, area: Rect) -> [Rect; 3] {
        let footer_props = self.footer_props();
        let footer_hint_height = if footer_props.is_session_mode {
            3 // model status + shortcut hints + path info
        } else {
            self.custom_footer_height()
                .unwrap_or_else(|| footer_height(footer_props.clone()))
        };
        let footer_spacing = Self::footer_spacing(footer_hint_height);
        let footer_total_height = footer_hint_height + footer_spacing;
        let top_inset = Self::composer_top_inset(footer_props.is_session_mode);
        let popup_constraint = match &self.active_popup {
            ActivePopup::Command(popup) => {
                Constraint::Max(popup.calculate_required_height(area.width))
            }
            ActivePopup::File(popup) => Constraint::Max(popup.calculate_required_height()),
            ActivePopup::Skill(popup) => {
                Constraint::Max(popup.calculate_required_height(area.width))
            }
            ActivePopup::None => Constraint::Max(footer_total_height),
        };
        let [composer_rect, popup_rect] =
            Layout::vertical([Constraint::Min(3), popup_constraint]).areas(area);
        let textarea_rect = composer_rect.inset(Insets::tlbr(top_inset, LIVE_PREFIX_COLS, 1, 1));
        [composer_rect, textarea_rect, popup_rect]
    }

    pub(super) fn composer_top_inset(is_session_mode: bool) -> u16 {
        if is_session_mode { 1 } else { 2 }
    }

    pub(super) fn footer_spacing(footer_hint_height: u16) -> u16 {
        if footer_hint_height == 0 {
            0
        } else {
            FOOTER_SPACING_HEIGHT
        }
    }

    /// Override the footer hint items displayed beneath the composer. Passing
    /// `None` restores the default shortcut footer.
    pub(crate) fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.footer_hint_override = items;
    }

    #[cfg(test)]
    pub(crate) fn show_footer_flash(&mut self, line: Line<'static>, duration: std::time::Duration) {
        use super::FooterFlash;
        let expires_at = std::time::Instant::now()
            .checked_add(duration)
            .unwrap_or_else(std::time::Instant::now);
        self.footer_flash = Some(FooterFlash { line, expires_at });
    }

    pub(crate) fn footer_flash_visible(&self) -> bool {
        self.footer_flash
            .as_ref()
            .is_some_and(|flash| std::time::Instant::now() < flash.expires_at)
    }

    pub(super) fn footer_props(&self) -> FooterProps {
        let mode = self.footer_mode();
        let is_wsl = {
            #[cfg(target_os = "linux")]
            {
                mode == FooterMode::ShortcutOverlay && crate::clipboard_paste::is_probably_wsl()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        };

        FooterProps {
            mode,
            esc_backtrack_hint: self.esc_backtrack_hint,
            use_shift_enter_hint: self.use_shift_enter_hint,
            is_task_running: self.is_task_running,
            quit_shortcut_key: self.quit_shortcut_key,
            steer_enabled: self.steer_enabled,
            collaboration_modes_enabled: self.collaboration_modes_enabled,
            is_wsl,
            context_window_percent: self.context_window_percent,
            context_window_used_tokens: self.context_window_used_tokens,
            model_display: self.model_display.clone(),
            provider_display: self.provider_display.clone(),
            cwd_display: self.cwd_display.clone(),
            is_session_mode: self.is_session_mode,
        }
    }

    /// Resolve the effective footer mode via a small priority waterfall.
    ///
    /// The base mode is derived solely from whether the composer is empty:
    /// `ComposerEmpty` iff empty, otherwise `ComposerHasDraft`. Transient
    /// modes (Esc hint, overlay, quit reminder) can override that base when
    /// their conditions are active.
    pub(super) fn footer_mode(&self) -> FooterMode {
        let base_mode = if self.is_empty() {
            FooterMode::ComposerEmpty
        } else {
            FooterMode::ComposerHasDraft
        };

        match self.footer_mode {
            FooterMode::EscHint => FooterMode::EscHint,
            FooterMode::ShortcutOverlay => FooterMode::ShortcutOverlay,
            FooterMode::QuitShortcutReminder if self.quit_shortcut_hint_visible() => {
                FooterMode::QuitShortcutReminder
            }
            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
                if self.quit_shortcut_hint_visible() =>
            {
                FooterMode::QuitShortcutReminder
            }
            FooterMode::QuitShortcutReminder => base_mode,
            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft => base_mode,
        }
    }

    pub(super) fn custom_footer_height(&self) -> Option<u16> {
        if self.footer_flash_visible() {
            return Some(1);
        }
        self.footer_hint_override
            .as_ref()
            .map(|items| if items.is_empty() { 0 } else { 1 })
    }

    pub(crate) fn render_with_mask(&self, area: Rect, buf: &mut Buffer, mask_char: Option<char>) {
        let [composer_rect, textarea_rect, popup_rect] = self.layout_areas(area);
        match &self.active_popup {
            ActivePopup::Command(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::File(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::Skill(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::None => {
                let footer_props = self.footer_props();

                if footer_props.is_session_mode {
                    // -- Session-mode footer (opencode-style) --
                    //
                    // Line 1: model  provider
                    // Line 2: tab agents  / commands  (or esc interrupt when running)
                    // Line 3: path info
                    //
                    // Special footer modes (esc hint, shortcut overlay, quit reminder)
                    // replace line 2 with their own content.

                    // Split popup_rect into up to 3 lines.
                    let (line1_rect, line2_rect, line3_rect) = if popup_rect.height >= 3 {
                        let [l1, l2, l3] = Layout::vertical([
                            Constraint::Length(1),
                            Constraint::Length(1),
                            Constraint::Length(1),
                        ])
                        .areas(popup_rect);
                        (l1, l2, l3)
                    } else if popup_rect.height >= 2 {
                        let [l1, l2] =
                            Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
                                .areas(popup_rect);
                        (l1, l2, Rect::ZERO)
                    } else if popup_rect.height >= 1 {
                        (
                            Rect::new(popup_rect.x, popup_rect.y, popup_rect.width, 1),
                            Rect::ZERO,
                            Rect::ZERO,
                        )
                    } else {
                        (Rect::ZERO, Rect::ZERO, Rect::ZERO)
                    };

                    render_model_status_line(
                        line1_rect,
                        buf,
                        &footer_props.model_display,
                        &footer_props.provider_display,
                    );

                    // Line 2: default to shortcut hints, but honour special footer modes.
                    if self.footer_flash_visible() {
                        if let Some(flash) = self.footer_flash.as_ref() {
                            flash.line.render(inset_footer_hint_area(line2_rect), buf);
                        }
                    } else if let Some(items) = self.footer_hint_override.as_ref() {
                        render_footer_hint_items(line2_rect, buf, items);
                    } else {
                        match footer_props.mode {
                            FooterMode::EscHint
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay => {
                                render_footer_from_props(
                                    line2_rect,
                                    buf,
                                    footer_props.clone(),
                                    self.collaboration_mode_indicator,
                                    false,
                                    false,
                                    false,
                                );
                            }
                            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft => {
                                render_shortcut_hints_line(
                                    line2_rect,
                                    buf,
                                    self.is_task_running,
                                    false,
                                );
                            }
                        }
                    }

                    // Line 3: path info.
                    render_path_info_line(line3_rect, buf, &footer_props.cwd_display);
                } else {
                    // -- Startup-mode footer (existing right-aligned context line) --
                    let show_cycle_hint = !footer_props.is_task_running
                        && self.collaboration_mode_indicator.is_some();
                    let show_shortcuts_hint = match footer_props.mode {
                        FooterMode::ComposerEmpty => !self.is_in_paste_burst(),
                        FooterMode::QuitShortcutReminder
                        | FooterMode::ShortcutOverlay
                        | FooterMode::EscHint
                        | FooterMode::ComposerHasDraft => false,
                    };
                    let show_queue_hint = match footer_props.mode {
                        FooterMode::ComposerHasDraft => {
                            footer_props.is_task_running && footer_props.steer_enabled
                        }
                        FooterMode::QuitShortcutReminder
                        | FooterMode::ComposerEmpty
                        | FooterMode::ShortcutOverlay
                        | FooterMode::EscHint => false,
                    };
                    let context_line = context_window_line(
                        footer_props.context_window_percent,
                        footer_props.context_window_used_tokens,
                        "",
                        "",
                        "",
                    );
                    let context_width = context_line.width() as u16;
                    let custom_height = self.custom_footer_height();
                    let footer_hint_height =
                        custom_height.unwrap_or_else(|| footer_height(footer_props.clone()));
                    let footer_spacing = Self::footer_spacing(footer_hint_height);
                    let hint_rect = if footer_spacing > 0 && footer_hint_height > 0 {
                        let [_, hint_rect] = Layout::vertical([
                            Constraint::Length(footer_spacing),
                            Constraint::Length(footer_hint_height),
                        ])
                        .areas(popup_rect);
                        hint_rect
                    } else {
                        popup_rect
                    };
                    let left_width = if self.footer_flash_visible() {
                        self.footer_flash
                            .as_ref()
                            .map(|flash| flash.line.width() as u16)
                            .unwrap_or(0)
                    } else if let Some(items) = self.footer_hint_override.as_ref() {
                        footer_hint_items_width(items)
                    } else {
                        footer_line_width(
                            footer_props.clone(),
                            self.collaboration_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        )
                    };
                    let can_show_left_and_context =
                        can_show_left_with_context(hint_rect, left_width, context_width);
                    let has_override =
                        self.footer_flash_visible() || self.footer_hint_override.is_some();
                    let single_line_layout = if has_override {
                        None
                    } else {
                        match footer_props.mode {
                            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft => {
                                Some(single_line_footer_layout(
                                    hint_rect,
                                    context_width,
                                    self.collaboration_mode_indicator,
                                    show_cycle_hint,
                                    show_shortcuts_hint,
                                    show_queue_hint,
                                ))
                            }
                            FooterMode::EscHint
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay => None,
                        }
                    };
                    let show_context = if matches!(
                        footer_props.mode,
                        FooterMode::EscHint
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay
                    ) {
                        false
                    } else {
                        single_line_layout
                            .as_ref()
                            .map(|(_, show_context)| *show_context)
                            .unwrap_or(can_show_left_and_context)
                    };

                    if let Some((summary_left, _)) = single_line_layout {
                        match summary_left {
                            SummaryLeft::Default => {
                                render_footer_from_props(
                                    hint_rect,
                                    buf,
                                    footer_props.clone(),
                                    self.collaboration_mode_indicator,
                                    show_cycle_hint,
                                    show_shortcuts_hint,
                                    show_queue_hint,
                                );
                            }
                            SummaryLeft::Custom(line) => {
                                render_footer_line(hint_rect, buf, line);
                            }
                            SummaryLeft::None => {}
                        }
                    } else if self.footer_flash_visible() {
                        if let Some(flash) = self.footer_flash.as_ref() {
                            flash.line.render(inset_footer_hint_area(hint_rect), buf);
                        }
                    } else if let Some(items) = self.footer_hint_override.as_ref() {
                        render_footer_hint_items(hint_rect, buf, items);
                    } else {
                        render_footer_from_props(
                            hint_rect,
                            buf,
                            footer_props,
                            self.collaboration_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        );
                    }

                    if show_context {
                        render_context_right(hint_rect, buf, &context_line);
                    }
                }
            }
        }
        let terminal_bg = crate::terminal_palette::default_bg();
        let border_color = self.border_color.unwrap_or_else(|| {
            // Default border color based on mode
            if self.is_task_running {
                Color::Yellow
            } else {
                Color::Blue
            }
        });
        let composer_bg = terminal_bg.map(input_bg).unwrap_or(Color::Reset);
        let cursor_color = terminal_bg.map(cursor_fg).unwrap_or(border_color);
        let style = user_message_style().bg(composer_bg);
        // Fill the composer background.
        (&Block::default().style(style)).render_ref(composer_rect, buf);
        if !self.is_session_mode && composer_rect.height > 0 {
            let header_area = Rect::new(composer_rect.x, composer_rect.y, composer_rect.width, 1);
            if !self.cwd_display.is_empty() {
                Line::from(vec![Span::from(self.cwd_display.clone()).dim()])
                    .render(header_area, buf);
            }

            let mut status_spans: Vec<Span<'static>> = Vec::new();
            if !self.model_display.is_empty() {
                status_spans.push(Span::from(self.model_display.clone()));
            }
            if !self.provider_display.is_empty() {
                status_spans.push(Span::from(" ").dim());
                status_spans.push(Span::from(self.provider_display.clone()).dim());
            }
            if !status_spans.is_empty() {
                let status_line = Line::from(status_spans);
                render_context_right(header_area, buf, &status_line);
            }

            let rule_style = Style::default().fg(border_color).bg(composer_bg).dim();
            let rule = "\u{2500}".repeat(composer_rect.width as usize);
            if composer_rect.height > 1 {
                let top_rule = Span::styled(rule.clone(), rule_style);
                buf.set_span(
                    composer_rect.x,
                    composer_rect.y + 1,
                    &top_rule,
                    composer_rect.width,
                );
            }
            if composer_rect.height > 2 {
                let bottom_rule_y = composer_rect.y + composer_rect.height - 1;
                let bottom_rule = Span::styled(rule, rule_style);
                buf.set_span(
                    composer_rect.x,
                    bottom_rule_y,
                    &bottom_rule,
                    composer_rect.width,
                );
            }
        }
        // Draw the cursor prompt on the first textarea row.
        if !textarea_rect.is_empty() {
            let prompt = if self.input_enabled {
                if self.is_session_mode {
                    " ".repeat(LIVE_PREFIX_COLS as usize)
                        .fg(cursor_color)
                        .bg(composer_bg)
                } else {
                    "\u{276F} ".fg(cursor_color).bg(composer_bg)
                }
            } else {
                " ".repeat(LIVE_PREFIX_COLS as usize).dim().bg(composer_bg)
            };
            buf.set_span(
                textarea_rect.x - LIVE_PREFIX_COLS,
                textarea_rect.y,
                &prompt,
                LIVE_PREFIX_COLS,
            );
        }

        let mut state = self.textarea_state.borrow_mut();
        if let Some(mask_char) = mask_char {
            self.textarea
                .render_ref_masked(textarea_rect, buf, &mut state, mask_char);
        } else {
            StatefulWidgetRef::render_ref(&(&self.textarea), textarea_rect, buf, &mut state);
        }
        if self.textarea.text().is_empty() {
            let text = if self.input_enabled {
                if self.is_session_mode {
                    self.placeholder_text.as_str().to_string()
                } else {
                    STARTUP_PLACEHOLDER_TEXT.to_string()
                }
            } else {
                self.input_disabled_placeholder
                    .as_deref()
                    .unwrap_or("Input disabled.")
                    .to_string()
            };
            if !textarea_rect.is_empty() {
                let placeholder = Span::from(text).dim();
                (&Line::from(vec![placeholder]))
                    .render_ref(textarea_rect.inner(Margin::new(0, 0)), buf);
            }
        }
    }
}

impl Renderable for ChatComposer {
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if !self.input_enabled {
            return None;
        }

        let [_, textarea_rect, _] = self.layout_areas(area);
        let state = *self.textarea_state.borrow();
        self.textarea.cursor_pos_with_state(textarea_rect, state)
    }

    fn desired_height(&self, width: u16) -> u16 {
        let footer_props = self.footer_props();
        let footer_hint_height = if footer_props.is_session_mode {
            // Session mode: 3 lines (model status + shortcut hints + path info).
            3
        } else {
            self.custom_footer_height()
                .unwrap_or_else(|| footer_height(footer_props.clone()))
        };
        let footer_spacing = Self::footer_spacing(footer_hint_height);
        let footer_total_height = footer_hint_height + footer_spacing;
        const COLS_WITH_MARGIN: u16 = LIVE_PREFIX_COLS + 1;
        let top_inset = Self::composer_top_inset(footer_props.is_session_mode);
        self.textarea
            .desired_height(width.saturating_sub(COLS_WITH_MARGIN))
            + top_inset
            + 1
            + match &self.active_popup {
                ActivePopup::None => footer_total_height,
                ActivePopup::Command(c) => c.calculate_required_height(width),
                ActivePopup::File(c) => c.calculate_required_height(),
                ActivePopup::Skill(c) => c.calculate_required_height(width),
            }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_with_mask(area, buf, None);
    }
}
