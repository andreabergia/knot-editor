//! Completion requests, aggregation, and acceptance for one view.

use super::*;

impl EditorView {
    pub(crate) fn start_completion(
        &mut self,
        buffer: BufferHandle,
        providers: Vec<CompletionProviderRegistration>,
        cx: &mut Context<Self>,
    ) -> Vec<(CompletionProviderRegistration, CompletionRequest)> {
        self.dismiss_completion();
        let model = self.model.read(cx);
        if !model.is_editable() {
            return Vec::new();
        }
        let revision = model.revision();
        let line_start = model.line_range(self.cursor_line).start;
        let cursor_byte_offset = line_start + self.cursor_col;
        let line_prefix = model.read(line_start..cursor_byte_offset);
        let prefix_len = line_prefix
            .bytes()
            .rev()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            .count();
        let prefix_start = cursor_byte_offset - prefix_len;
        let generation = self.next_completion_generation;
        self.next_completion_generation = self
            .next_completion_generation
            .checked_add(1)
            .expect("completion generation space exhausted");
        let controller = CompletionController::new(
            buffer,
            revision,
            cursor_byte_offset,
            prefix_start..cursor_byte_offset,
            line_prefix[line_prefix.len() - prefix_len..].into(),
            generation,
            providers,
        );
        let requests = controller.requests();
        self.completion_surface = Some(CompletionSurface::attach(controller.snapshot().clone()));
        self.completion = Some(controller);
        cx.notify();
        requests
    }

    pub(crate) fn apply_completion_response(
        &mut self,
        buffer: BufferHandle,
        response: CompletionResponse,
        cx: &mut Context<Self>,
    ) -> bool {
        let revision = self.model.read(cx).revision();
        let Some(controller) = self.completion.as_mut() else {
            return false;
        };
        if controller.buffer() != buffer || controller.revision() != revision {
            return false;
        }
        if controller.apply_response(response) {
            if let Some(surface) = self.completion_surface.as_mut() {
                surface.update(controller.snapshot().clone());
            }
            cx.notify();
            true
        } else {
            false
        }
    }

    pub(crate) fn retain_completion_task(
        &mut self,
        generation: u64,
        registration: CompletionProviderRegistrationId,
        task: Task<()>,
    ) {
        if self
            .completion
            .as_ref()
            .is_some_and(|controller| controller.generation() == generation)
        {
            self.completion_tasks.push((registration, task));
        }
    }

    pub(crate) fn cancel_completion_provider(
        &mut self,
        registration: CompletionProviderRegistrationId,
        cx: &mut Context<Self>,
    ) {
        self.completion_tasks
            .retain(|(provider, _)| *provider != registration);
        if let Some(controller) = self.completion.as_mut()
            && controller.provider_unavailable(registration)
        {
            if let Some(surface) = self.completion_surface.as_mut() {
                surface.update(controller.snapshot().clone());
            }
            cx.notify();
        }
    }

    pub(super) fn dismiss_completion(&mut self) {
        self.completion_surface = None;
        self.completion = None;
        self.completion_tasks.clear();
    }

    pub(super) fn completion_anchor(
        &self,
        model: &BufferModel,
        window: &mut Window,
    ) -> Point<Pixels> {
        let line = self.cursor_line;
        let text = line_text(model, line);
        let x = if text.is_empty() {
            px(0.)
        } else {
            let shaped = shape_editor_line(
                window,
                SharedString::from(text.clone()),
                px(FONT_SIZE),
                &runs_for(&text),
            );
            self.line_x_offset(&text, shaped.width, self.bounds.size.width)
                + x_for_index_dir(&shaped, self.cursor_col, &text)
        };
        let y = px((line + 1) as f32 * LINE_HEIGHT - self.scroll);
        point(
            x.min((self.bounds.size.width - px(380.)).max(px(0.))),
            y.min((self.bounds.size.height - px(230.)).max(px(0.))),
        )
    }

    pub(super) fn completion_previous(
        &mut self,
        _: &CompletionPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(surface) = self.completion_surface.as_mut() {
            surface.move_selection(-1);
            cx.notify();
        }
    }

    pub(super) fn completion_next(
        &mut self,
        _: &CompletionNext,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(surface) = self.completion_surface.as_mut() {
            surface.move_selection(1);
            cx.notify();
        }
    }

    pub(super) fn completion_accept(
        &mut self,
        _: &CompletionAccept,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.accept_selected_completion(cx);
    }

    pub(super) fn accept_selected_completion(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .completion_surface
            .as_ref()
            .and_then(|surface| surface.selected_item());
        let accepted = selected.and_then(|selected| {
            let controller = self.completion.as_ref()?;
            let item = controller.item(selected)?;
            Some((
                controller.revision(),
                controller.prefix_range(),
                item.insert_text.clone(),
            ))
        });
        self.dismiss_completion();
        let Some((revision, range, insert_text)) = accepted else {
            cx.notify();
            return;
        };
        let edit = TextEdit {
            range: ByteRange {
                start_byte_offset: range.start,
                end_byte_offset: range.end,
            },
            text: insert_text,
        };
        self.model.update(cx, |model, cx| {
            if model.apply_edits(&[edit], revision).unwrap_or(false) {
                cx.notify();
            }
        });
    }

    pub(super) fn completion_dismiss(
        &mut self,
        _: &CompletionDismiss,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_completion();
        cx.notify();
    }
}
