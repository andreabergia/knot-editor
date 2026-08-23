use std::{collections::{HashMap, HashSet}, ops::Range, sync::Arc};

use gpui::{prelude::*, *};

use crate::host::protocol::{
    BufferHandle, CompletionProviderError, CompletionProviderRegistrationId, CompletionRequest,
    CompletionResponse, CompletionResultItem, ExtensionId, ExtensionLifecycleId,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CompletionItemId(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompletionItem {
    pub(crate) id: CompletionItemId,
    pub(crate) label: String,
    pub(crate) insert_text: String,
    pub(crate) provider_label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompletionFailure {
    pub(crate) provider_label: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompletionSnapshot {
    pub(crate) items: Arc<[CompletionItem]>,
    pub(crate) pending_provider_count: usize,
    pub(crate) failures: Arc<[CompletionFailure]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompletionSurfaceKind {
    List,
    Compact,
}

pub(crate) trait CompletionSurface {
    fn kind(&self) -> CompletionSurfaceKind;
    fn update(&mut self, snapshot: CompletionSnapshot);
    fn move_selection(&mut self, delta: isize);
    fn selected_item(&self) -> Option<CompletionItemId>;
    fn render(&self, anchor: Point<Pixels>) -> AnyElement;
    fn detach(&mut self);
}

fn initial_selection(snapshot: &CompletionSnapshot) -> Option<CompletionItemId> {
    snapshot.items.first().map(|item| item.id)
}

fn move_selection(
    snapshot: &CompletionSnapshot,
    selected: &mut Option<CompletionItemId>,
    delta: isize,
) {
    if snapshot.items.is_empty() {
        *selected = None;
        return;
    }
    let index = selected
        .and_then(|selected| snapshot.items.iter().position(|item| item.id == selected))
        .unwrap_or(0);
    let last = snapshot.items.len().saturating_sub(1) as isize;
    let next = (index as isize + delta).clamp(0, last) as usize;
    *selected = Some(snapshot.items[next].id);
}

fn preserve_selection(snapshot: &CompletionSnapshot, selected: &mut Option<CompletionItemId>) {
    if selected.is_none_or(|selected| !snapshot.items.iter().any(|item| item.id == selected)) {
        *selected = initial_selection(snapshot);
    }
}

pub(crate) struct ListCompletionSurface {
    snapshot: CompletionSnapshot,
    selected: Option<CompletionItemId>,
}

impl ListCompletionSurface {
    pub(crate) fn attach(snapshot: CompletionSnapshot) -> Self {
        let selected = initial_selection(&snapshot);
        Self { snapshot, selected }
    }
}

impl CompletionSurface for ListCompletionSurface {
    fn kind(&self) -> CompletionSurfaceKind {
        CompletionSurfaceKind::List
    }

    fn update(&mut self, snapshot: CompletionSnapshot) {
        self.snapshot = snapshot;
        preserve_selection(&self.snapshot, &mut self.selected);
    }

    fn move_selection(&mut self, delta: isize) {
        move_selection(&self.snapshot, &mut self.selected, delta);
    }

    fn selected_item(&self) -> Option<CompletionItemId> {
        self.selected
    }

    fn render(&self, anchor: Point<Pixels>) -> AnyElement {
        let selected = self.selected;
        let status = format!(
            "{} pending · {} failed",
            self.snapshot.pending_provider_count,
            self.snapshot.failures.len()
        );
        div()
            .absolute()
            .left(anchor.x)
            .top(anchor.y)
            .w(px(360.))
            .max_h(px(220.))
            .overflow_hidden()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x4c566a))
            .bg(rgb(0x252a34))
            .text_sm()
            .children(self.snapshot.items.iter().take(8).map(|item| {
                div()
                    .flex()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .when(Some(item.id) == selected, |row| row.bg(rgb(0x3b4252)))
                    .child(item.label.clone())
                    .child(
                        div()
                            .text_color(rgb(0x88c0d0))
                            .child(item.provider_label.clone()),
                    )
            }))
            .children(self.snapshot.failures.iter().map(|failure| {
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(0xbf616a))
                    .child(format!(
                        "{}: {}",
                        failure.provider_label, failure.message
                    ))
            }))
            .child(
                div()
                    .border_t_1()
                    .border_color(rgb(0x4c566a))
                    .px_2()
                    .py_1()
                    .text_color(rgb(0xa3a3a3))
                    .child(status),
            )
            .into_any_element()
    }

    fn detach(&mut self) {
        self.selected = None;
    }
}

pub(crate) struct CompactCompletionSurface {
    snapshot: CompletionSnapshot,
    selected: Option<CompletionItemId>,
}

impl CompactCompletionSurface {
    pub(crate) fn attach(snapshot: CompletionSnapshot) -> Self {
        let selected = initial_selection(&snapshot);
        Self { snapshot, selected }
    }
}

impl CompletionSurface for CompactCompletionSurface {
    fn kind(&self) -> CompletionSurfaceKind {
        CompletionSurfaceKind::Compact
    }

    fn update(&mut self, snapshot: CompletionSnapshot) {
        self.snapshot = snapshot;
        preserve_selection(&self.snapshot, &mut self.selected);
    }

    fn move_selection(&mut self, delta: isize) {
        move_selection(&self.snapshot, &mut self.selected, delta);
    }

    fn selected_item(&self) -> Option<CompletionItemId> {
        self.selected
    }

    fn render(&self, anchor: Point<Pixels>) -> AnyElement {
        let candidate = self
            .selected
            .and_then(|selected| self.snapshot.items.iter().find(|item| item.id == selected))
            .map_or_else(|| "waiting…".into(), |item| item.label.clone());
        let status = format!(
            "{} items / {} pending / {} failed",
            self.snapshot.items.len(),
            self.snapshot.pending_provider_count,
            self.snapshot.failures.len()
        );
        div()
            .absolute()
            .left(anchor.x)
            .top(anchor.y)
            .flex()
            .items_center()
            .gap_2()
            .rounded_full()
            .border_1()
            .border_color(rgb(0xd08770))
            .bg(rgb(0x3b2f2f))
            .px_3()
            .py_1()
            .text_sm()
            .child(candidate)
            .child(div().text_xs().text_color(rgb(0xd8a48f)).child(status))
            .into_any_element()
    }

    fn detach(&mut self) {
        self.selected = None;
    }
}

#[derive(Clone, Debug)]
struct ProviderItem {
    id: CompletionItemId,
    item: CompletionResultItem,
    local_order: usize,
}

#[derive(Clone, Debug)]
enum ProviderState {
    Pending,
    Successful(Vec<ProviderItem>),
    Failed(String),
}

#[derive(Clone, Debug)]
struct SessionProvider {
    registration: CompletionProviderRegistration,
    state: ProviderState,
}

pub(crate) struct CompletionController {
    buffer: BufferHandle,
    revision: u64,
    cursor_byte_offset: usize,
    prefix_range: Range<usize>,
    prefix: String,
    generation: u64,
    next_item_id: u64,
    providers: HashMap<CompletionProviderRegistrationId, SessionProvider>,
    snapshot: CompletionSnapshot,
}

impl CompletionController {
    pub(crate) fn new(
        buffer: BufferHandle,
        revision: u64,
        cursor_byte_offset: usize,
        prefix_range: Range<usize>,
        prefix: String,
        generation: u64,
        registrations: Vec<CompletionProviderRegistration>,
    ) -> Self {
        let pending_provider_count = registrations.len();
        let providers = registrations
            .into_iter()
            .map(|registration| {
                (
                    registration.id,
                    SessionProvider {
                        registration,
                        state: ProviderState::Pending,
                    },
                )
            })
            .collect();
        Self {
            buffer,
            revision,
            cursor_byte_offset,
            prefix_range,
            prefix,
            generation,
            next_item_id: 1,
            providers,
            snapshot: CompletionSnapshot {
                items: Arc::from([]),
                pending_provider_count,
                failures: Arc::from([]),
            },
        }
    }

    pub(crate) fn requests(&self) -> Vec<(CompletionProviderRegistration, CompletionRequest)> {
        let mut providers = self
            .providers
            .values()
            .map(|provider| provider.registration.clone())
            .collect::<Vec<_>>();
        providers.sort_by_key(|provider| provider.order);
        providers
            .into_iter()
            .map(|provider| {
                let request = CompletionRequest {
                    registration: provider.id,
                    buffer: self.buffer,
                    revision: self.revision,
                    cursor_byte_offset: self.cursor_byte_offset,
                    prefix: self.prefix.clone(),
                    generation: self.generation,
                };
                (provider, request)
            })
            .collect()
    }

    pub(crate) fn snapshot(&self) -> &CompletionSnapshot {
        &self.snapshot
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn buffer(&self) -> BufferHandle {
        self.buffer
    }

    pub(crate) fn prefix_range(&self) -> Range<usize> {
        self.prefix_range.clone()
    }

    pub(crate) fn item(&self, id: CompletionItemId) -> Option<&CompletionItem> {
        self.snapshot.items.iter().find(|item| item.id == id)
    }

    pub(crate) fn apply_response(&mut self, response: CompletionResponse) -> bool {
        if response.revision != self.revision || response.generation != self.generation {
            return false;
        }
        let Some(provider) = self.providers.get_mut(&response.registration) else {
            return false;
        };
        if !matches!(provider.state, ProviderState::Pending) {
            return false;
        }
        provider.state = match response.result {
            Ok(items) => ProviderState::Successful(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(local_order, item)| {
                        let id = CompletionItemId(self.next_item_id);
                        self.next_item_id = self
                            .next_item_id
                            .checked_add(1)
                            .expect("completion item identity space exhausted");
                        ProviderItem {
                            id,
                            item,
                            local_order,
                        }
                    })
                    .collect(),
            ),
            Err(error) => ProviderState::Failed(error.message),
        };
        self.rebuild_snapshot();
        true
    }

    pub(crate) fn provider_unavailable(
        &mut self,
        registration: CompletionProviderRegistrationId,
    ) -> bool {
        self.apply_response(CompletionResponse {
            registration,
            revision: self.revision,
            generation: self.generation,
            result: Err(CompletionProviderError {
                message: "provider lifecycle ended".into(),
            }),
        })
    }

    fn rebuild_snapshot(&mut self) {
        let mut ranked = Vec::new();
        let mut failures = Vec::new();
        let mut pending_provider_count = 0;
        for provider in self.providers.values() {
            match &provider.state {
                ProviderState::Pending => pending_provider_count += 1,
                ProviderState::Failed(message) => failures.push((
                    provider.registration.order,
                    CompletionFailure {
                        provider_label: provider.registration.label.clone(),
                        message: message.clone(),
                    },
                )),
                ProviderState::Successful(items) => {
                    for item in items {
                        let match_rank = if item.item.label.starts_with(&self.prefix) {
                            0
                        } else if item
                            .item
                            .label
                            .get(..self.prefix.len())
                            .is_some_and(|start| start.eq_ignore_ascii_case(&self.prefix))
                        {
                            1
                        } else {
                            continue;
                        };
                        ranked.push((
                            match_rank,
                            provider.registration.order,
                            item.local_order,
                            CompletionItem {
                                id: item.id,
                                label: item.item.label.clone(),
                                insert_text: item.item.insert_text.clone(),
                                provider_label: provider.registration.label.clone(),
                            },
                        ));
                    }
                }
            }
        }
        ranked.sort_by_key(|(match_rank, provider_order, local_order, _)| {
            (*match_rank, *provider_order, *local_order)
        });
        failures.sort_by_key(|(provider_order, _)| *provider_order);
        let mut insertion_texts = HashSet::new();
        self.snapshot = CompletionSnapshot {
            items: ranked
                .into_iter()
                .filter_map(|(_, _, _, item)| {
                    insertion_texts.insert(item.insert_text.clone()).then_some(item)
                })
                .collect::<Vec<_>>()
                .into(),
            pending_provider_count,
            failures: failures
                .into_iter()
                .map(|(_, failure)| failure)
                .collect::<Vec<_>>()
                .into(),
        };
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompletionProviderRegistration {
    pub(crate) id: CompletionProviderRegistrationId,
    pub(crate) extension: ExtensionId,
    pub(crate) lifecycle: ExtensionLifecycleId,
    pub(crate) label: String,
    pub(crate) order: u64,
}

pub(crate) struct CompletionProviderRegistry {
    next_registration: u64,
    registrations: HashMap<CompletionProviderRegistrationId, CompletionProviderRegistration>,
}

impl CompletionProviderRegistry {
    pub(crate) fn new() -> Self {
        Self {
            next_registration: 1,
            registrations: HashMap::new(),
        }
    }

    pub(crate) fn register(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        label: String,
    ) -> CompletionProviderRegistrationId {
        let order = self.next_registration;
        self.next_registration = self
            .next_registration
            .checked_add(1)
            .expect("completion registration space exhausted");
        let id = CompletionProviderRegistrationId::new(order);
        self.registrations.insert(
            id,
            CompletionProviderRegistration {
                id,
                extension,
                lifecycle,
                label,
                order,
            },
        );
        id
    }

    pub(crate) fn unregister(
        &mut self,
        id: CompletionProviderRegistrationId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> bool {
        if !self.owns(id, extension, lifecycle) {
            return false;
        }
        self.registrations.remove(&id);
        true
    }

    pub(crate) fn owns(
        &self,
        id: CompletionProviderRegistrationId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> bool {
        self.registrations.get(&id).is_some_and(|registration| {
            registration.extension == extension && registration.lifecycle == lifecycle
        })
    }

    pub(crate) fn snapshot(&self) -> Vec<CompletionProviderRegistration> {
        let mut registrations = self.registrations.values().cloned().collect::<Vec<_>>();
        registrations.sort_by_key(|registration| registration.order);
        registrations
    }

    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> Vec<CompletionProviderRegistrationId> {
        let removed = self
            .registrations
            .values()
            .filter(|registration| {
                registration.extension == extension && registration.lifecycle == lifecycle
            })
            .map(|registration| registration.id)
            .collect::<Vec<_>>();
        self.registrations.retain(|_, registration| {
            registration.extension != extension || registration.lifecycle != lifecycle
        });
        removed
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        CompactCompletionSurface, CompletionController, CompletionItem, CompletionItemId,
        CompletionProviderRegistry, CompletionSnapshot, CompletionSurface, ListCompletionSurface,
    };
    use crate::host::protocol::{
        BufferHandle, CompletionProviderError, CompletionResponse, CompletionResultItem,
        ExtensionId, ExtensionLifecycleId,
    };

    #[test]
    fn registration_snapshot_preserves_registration_order_and_lifecycle_ownership() {
        let mut registry = CompletionProviderRegistry::new();
        let first = registry.register(
            ExtensionId::new(2),
            ExtensionLifecycleId::new(1),
            "first".into(),
        );
        let second = registry.register(
            ExtensionId::new(1),
            ExtensionLifecycleId::new(3),
            "second".into(),
        );

        assert_eq!(
            registry
                .snapshot()
                .into_iter()
                .map(|registration| registration.id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(!registry.unregister(first, ExtensionId::new(1), ExtensionLifecycleId::new(3)));
        assert!(registry.owns(first, ExtensionId::new(2), ExtensionLifecycleId::new(1)));

        registry.remove_lifecycle(ExtensionId::new(2), ExtensionLifecycleId::new(1));
        assert!(!registry.owns(first, ExtensionId::new(2), ExtensionLifecycleId::new(1)));
    }

    #[test]
    fn merge_is_deterministic_across_arrival_order_and_deduplicates_insertion_text() {
        fn merged(reverse: bool) -> Vec<(String, String)> {
            let mut registry = CompletionProviderRegistry::new();
            let first = registry.register(
                ExtensionId::new(1),
                ExtensionLifecycleId::new(1),
                "first".into(),
            );
            let second = registry.register(
                ExtensionId::new(2),
                ExtensionLifecycleId::new(2),
                "second".into(),
            );
            let mut controller = CompletionController::new(
                BufferHandle::new(9),
                4,
                3,
                0..3,
                "foo".into(),
                7,
                registry.snapshot(),
            );
            let responses = [
                CompletionResponse {
                    registration: first,
                    revision: 4,
                    generation: 7,
                    result: Ok(vec![
                        CompletionResultItem {
                            label: "fooOne".into(),
                            insert_text: "shared".into(),
                        },
                        CompletionResultItem {
                            label: "FooTwo".into(),
                            insert_text: "two".into(),
                        },
                    ]),
                },
                CompletionResponse {
                    registration: second,
                    revision: 4,
                    generation: 7,
                    result: Ok(vec![
                        CompletionResultItem {
                            label: "fooThree".into(),
                            insert_text: "three".into(),
                        },
                        CompletionResultItem {
                            label: "fooDuplicate".into(),
                            insert_text: "shared".into(),
                        },
                        CompletionResultItem {
                            label: "bar".into(),
                            insert_text: "excluded".into(),
                        },
                    ]),
                },
            ];
            for response in if reverse {
                responses.into_iter().rev().collect::<Vec<_>>()
            } else {
                responses.into_iter().collect()
            } {
                assert!(controller.apply_response(response));
            }
            controller
                .snapshot()
                .items
                .iter()
                .map(|item| (item.label.clone(), item.provider_label.clone()))
                .collect()
        }

        let expected = vec![
            ("fooOne".into(), "first".into()),
            ("fooThree".into(), "second".into()),
            ("FooTwo".into(), "first".into()),
        ];
        assert_eq!(merged(false), expected);
        assert_eq!(merged(true), expected);
    }

    #[test]
    fn stale_duplicate_and_failed_responses_are_isolated() {
        let mut registry = CompletionProviderRegistry::new();
        let first = registry.register(
            ExtensionId::new(1),
            ExtensionLifecycleId::new(1),
            "first".into(),
        );
        let second = registry.register(
            ExtensionId::new(2),
            ExtensionLifecycleId::new(2),
            "second".into(),
        );
        let mut controller = CompletionController::new(
            BufferHandle::new(1),
            2,
            0,
            0..0,
            String::new(),
            3,
            registry.snapshot(),
        );

        assert!(!controller.apply_response(CompletionResponse {
            registration: first,
            revision: 1,
            generation: 3,
            result: Ok(Vec::new()),
        }));
        assert!(controller.apply_response(CompletionResponse {
            registration: first,
            revision: 2,
            generation: 3,
            result: Ok(vec![CompletionResultItem {
                label: "visible".into(),
                insert_text: "visible".into(),
            }]),
        }));
        assert!(!controller.apply_response(CompletionResponse {
            registration: first,
            revision: 2,
            generation: 3,
            result: Ok(Vec::new()),
        }));
        assert!(controller.apply_response(CompletionResponse {
            registration: second,
            revision: 2,
            generation: 3,
            result: Err(CompletionProviderError {
                message: "recoverable".into(),
            }),
        }));

        assert_eq!(controller.snapshot().items[0].label, "visible");
        assert_eq!(controller.snapshot().pending_provider_count, 0);
        assert_eq!(controller.snapshot().failures[0].provider_label, "second");
    }

    #[test]
    fn surface_updates_preserve_selection_while_replacement_resets_it() {
        fn snapshot(items: &[(u64, &str)]) -> CompletionSnapshot {
            CompletionSnapshot {
                items: items
                    .iter()
                    .map(|(id, label)| CompletionItem {
                        id: CompletionItemId(*id),
                        label: (*label).into(),
                        insert_text: (*label).into(),
                        provider_label: "fixture".into(),
                    })
                    .collect::<Vec<_>>()
                    .into(),
                pending_provider_count: 0,
                failures: Arc::from([]),
            }
        }

        let mut list = ListCompletionSurface::attach(snapshot(&[(1, "one"), (2, "two")]));
        list.move_selection(1);
        assert_eq!(list.selected_item(), Some(CompletionItemId(2)));
        let reordered = snapshot(&[(3, "three"), (2, "two"), (1, "one")]);
        list.update(reordered.clone());
        assert_eq!(list.selected_item(), Some(CompletionItemId(2)));

        let compact = CompactCompletionSurface::attach(reordered);
        assert_eq!(compact.selected_item(), Some(CompletionItemId(3)));
    }
}
