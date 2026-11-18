//! The Settings window: the AI keys (kept in the computer's credential store, never in a file), saved AI answers, and the video tools.

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ve_core::ai::{self, Provider};
use ve_core::store;

use crate::controls::{disabled, primary_button, secondary_button, small_button, switch};
use crate::theme::{self, ACCENT, BAD, LINE_STRONG, OK, TEXT3, WARN, colour, tint};
use crate::{Shell, theme::CARD_DEEP};

pub struct SettingsUi {
    pub open: bool,
    openai: Entity<InputState>,
    gemini: Entity<InputState>,
}

impl SettingsUi {
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        // The keys are hidden as they are typed, and the boxes are emptied once a key is saved.
        let openai = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Paste your OpenAI key"));
        let gemini = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Paste your Gemini key"));
        Self { open: false, openai, gemini }
    }
}

impl Shell {
    /// A line saying a key is missing, with a button that opens Settings.
    pub fn key_hint(&self, text: &str, id: &'static str, cx: &mut Context<Self>) -> Div {
        div().flex().items_center().gap(px(10.0)).child(div().flex_1().min_w(px(0.0)).child(theme::muted(text.to_string()))).child(small_button(id, "Open Settings").on_click(cx.listener(|this, _, _, cx| {
            this.settings_ui.open = true;
            cx.notify();
        })))
    }

    fn key_box(&self, provider: Provider, cx: &mut Context<Self>) -> Div {
        let (name, hint, has, input) = match provider {
            Provider::OpenAi => ("OpenAI", "Montage plans, upload packs, thumbnails and look suggestions", self.engine.has_openai, &self.settings_ui.openai),
            Provider::Gemini => ("Gemini", "Optional: copies a look from Gemini Omni", self.engine.has_gemini, &self.settings_ui.gemini),
        };
        let typed = !input.read(cx).value().trim().is_empty();
        let save = {
            let b = small_button(("key-save", provider as usize), "Save");
            if typed {
                b.on_click(cx.listener(move |this, _, window, cx| {
                    let state = if provider == Provider::OpenAi { this.settings_ui.openai.clone() } else { this.settings_ui.gemini.clone() };
                    let text = state.read(cx).value().to_string();
                    match ai::save_key(provider, &text) {
                        Ok(()) => {
                            state.update(cx, |s, cx| s.set_value("", window, cx));
                            this.engine.refresh_keys();
                            this.engine.note(format!("Your {} key is saved in the credential store.", if provider == Provider::OpenAi { "OpenAI" } else { "Gemini" }));
                        }
                        Err(e) => this.engine.fail(e),
                    }
                    cx.notify();
                }))
            } else {
                disabled(b)
            }
        };
        theme::card()
            .bg(colour(CARD_DEEP))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(div().flex().items_center().justify_between().child(theme::heading(name, 14.0)).child(if has { theme::status_pill("Saved", OK) } else { theme::status_pill("Not added", TEXT3) }))
            .child(theme::muted(hint))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().flex_1().min_w(px(0.0)).child(Input::new(input)))
                    .child(save)
                    .when(has, |s| {
                        s.child(small_button(("key-remove", provider as usize), "Remove").on_click(cx.listener(move |this, _, _, cx| {
                            let _ = ai::save_key(provider, "");
                            this.engine.refresh_keys();
                            this.engine.note(format!("Your {} key was removed.", if provider == Provider::OpenAi { "OpenAI" } else { "Gemini" }));
                            cx.notify();
                        })))
                    }),
            )
    }

    pub fn settings_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let e = &self.engine;
        let tools = if e.tools.is_none() {
            let mut card = theme::card().bg(tint(WARN, 0.07)).border_color(tint(WARN, 0.27)).flex().flex_col().gap(px(8.0)).child(theme::heading("Video tools needed", 14.0)).child(theme::soft("FFmpeg reads and writes your video. Download it once (about 110 MB, checked for safety before use)."));
            card = match e.tools_download {
                Some(p) => card.child(div().h(px(8.0)).rounded(px(4.0)).bg(colour(0x33343A)).child(div().h(px(8.0)).rounded(px(4.0)).bg(colour(ACCENT)).w(relative(p.clamp(0.0, 1.0))))),
                None => card.child(div().flex().child(primary_button("settings-tools", "Download video tools").on_click(cx.listener(|this, _, _, cx| {
                    this.engine.start_tools_download();
                    cx.notify();
                })))),
            };
            Some(card)
        } else {
            None
        };
        div()
            .id("settings-overlay")
            .occlude()
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .size_full()
            .bg(gpui_kit::black().opacity(0.55))
            .flex()
            .items_center()
            .justify_center()
            .on_click(|_, _, _| {})
            .child(
                theme::card()
                    .w(px(480.0))
                    .p(px(22.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(div().flex().items_center().justify_between().child(theme::heading("Settings", 17.0)).child(small_button("settings-close", "Close").on_click(cx.listener(|this, _, _, cx| {
                        this.settings_ui.open = false;
                        cx.notify();
                    }))))
                    .child(theme::heading("AI keys", 14.5))
                    .child(theme::muted("Stored in your computer's secure credential store, never in a file."))
                    .child(self.key_box(Provider::OpenAi, cx))
                    .child(self.key_box(Provider::Gemini, cx))
                    .child(div().pt(px(4.0)).child(theme::heading("Saved AI answers", 14.5)))
                    .child(switch("reuse-ai-settings", "Reuse a saved answer when nothing has changed (no tokens spent)", e.settings.reuse_ai, cx, |this| this.engine.settings.reuse_ai = !this.engine.settings.reuse_ai))
                    .child(div().flex().child(secondary_button("forget-answers", "Forget all saved answers").on_click(cx.listener(|this, _, _, cx| {
                        let n = store::clear_ai_cache();
                        this.engine.note(format!("Forgot {n} saved AI answer(s). The next request will ask the AI again."));
                        cx.notify();
                    }))))
                    .when_some(tools, |c, t| c.child(t)),
            )
    }
}

#[allow(dead_code)]
fn unused() -> (u32, u32) {
    (BAD, LINE_STRONG)
}
