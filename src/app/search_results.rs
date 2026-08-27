use gpui::{App, AppContext, Entity};

use crate::host::protocol::{ByteRange, DecorationToken, EditorContribution};

use super::{
    documents::DocumentId,
    model::{BufferModel, ContributionSource},
};

pub(crate) const ACTIVATE_SEARCH_RESULT_COMMAND: &str = "knot.search-result.activate";

/// One source match, independent of how search results are presented.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SearchMatch {
    pub source: DocumentId,
    pub source_range: ByteRange,
    pub line_number: usize,
    pub preview: String,
}

/// The formatter's association between visible text and one semantic match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EmittedSearchResult {
    pub output_range: ByteRange,
    pub match_index: usize,
}

#[derive(Clone)]
pub(crate) struct SearchResultTarget {
    pub source: DocumentId,
    pub source_model: Entity<BufferModel>,
    pub source_range: ByteRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchResultActivationError {
    UnknownResult,
    StaleSource,
}

#[derive(Debug, Eq, PartialEq)]
struct FormattedSearchResults {
    text: String,
    emitted_results: Vec<EmittedSearchResult>,
}

/// Owns an immutable generated search buffer and its semantic source targets.
#[allow(
    dead_code,
    reason = "source metadata is retained independently of rendered search text"
)]
pub(crate) struct SearchResultsController {
    query: String,
    source_model: Entity<BufferModel>,
    source_revision: u64,
    matches: Vec<SearchMatch>,
    emitted_results: Vec<EmittedSearchResult>,
    model: Entity<BufferModel>,
}

impl SearchResultsController {
    pub(crate) fn search(
        query: impl Into<String>,
        source: DocumentId,
        source_title: &str,
        source_model: Entity<BufferModel>,
        cx: &mut App,
    ) -> Self {
        let query = query.into();
        let (snapshot, source_revision) =
            source_model.read_with(cx, |model, _| (model.text(), model.revision()));
        let matches = find_matches(&snapshot, &query, source);
        let formatted = format_one_match_per_line(source_title, &matches);
        let contributions = formatted
            .emitted_results
            .iter()
            .map(|result| EditorContribution {
                range: result.output_range,
                decoration: Some(DecorationToken::Info),
                gutter: None,
                command: Some(ACTIVATE_SEARCH_RESULT_COMMAND.into()),
            })
            .collect::<Vec<_>>();
        let model = cx.new(|_| {
            let mut model = BufferModel::from_read_only_text(formatted.text);
            model
                .replace_contributions(ContributionSource::BuiltIn, &contributions, 0)
                .expect("formatter emits valid result ranges");
            model
        });

        Self {
            query,
            source_model,
            source_revision,
            matches,
            emitted_results: formatted.emitted_results,
            model,
        }
    }

    pub(crate) fn title(&self) -> String {
        format!("Search: \"{}\"", self.query)
    }

    pub(crate) fn model(&self) -> &Entity<BufferModel> {
        &self.model
    }

    pub(crate) fn resolve_target(
        &self,
        output_range: ByteRange,
        cx: &App,
    ) -> Result<SearchResultTarget, SearchResultActivationError> {
        let emitted = self
            .emitted_results
            .iter()
            .find(|result| result.output_range == output_range)
            .ok_or(SearchResultActivationError::UnknownResult)?;
        if self.source_model.read(cx).revision() != self.source_revision {
            return Err(SearchResultActivationError::StaleSource);
        }
        let result_match = &self.matches[emitted.match_index];
        Ok(SearchResultTarget {
            source: result_match.source,
            source_model: self.source_model.clone(),
            source_range: result_match.source_range,
        })
    }

    #[cfg(test)]
    pub(crate) fn source_model(&self) -> &Entity<BufferModel> {
        &self.source_model
    }

    #[cfg(test)]
    pub(crate) fn source_revision(&self) -> u64 {
        self.source_revision
    }

    #[cfg(test)]
    pub(crate) fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    #[cfg(test)]
    pub(crate) fn emitted_results(&self) -> &[EmittedSearchResult] {
        &self.emitted_results
    }
}

fn find_matches(text: &str, query: &str, source: DocumentId) -> Vec<SearchMatch> {
    if query.is_empty() {
        return Vec::new();
    }

    let line_starts = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(offset, _)| offset + 1))
        .collect::<Vec<_>>();
    text.match_indices(query)
        .map(|(start, matched)| {
            let line_index = line_starts.partition_point(|&line_start| line_start <= start) - 1;
            let line_start = line_starts[line_index];
            let line_end = text[line_start..]
                .find('\n')
                .map_or(text.len(), |offset| line_start + offset);
            SearchMatch {
                source,
                source_range: ByteRange {
                    start_byte_offset: start,
                    end_byte_offset: start + matched.len(),
                },
                line_number: line_index + 1,
                preview: text[line_start..line_end].to_owned(),
            }
        })
        .collect()
}

fn format_one_match_per_line(
    source_title: &str,
    matches: &[SearchMatch],
) -> FormattedSearchResults {
    let mut text = String::new();
    let mut emitted_results = Vec::with_capacity(matches.len());
    for (match_index, result_match) in matches.iter().enumerate() {
        if !text.is_empty() {
            text.push('\n');
        }
        let start = text.len();
        text.push_str(source_title);
        text.push(':');
        text.push_str(&result_match.line_number.to_string());
        text.push_str(": ");
        text.push_str(&result_match.preview);
        emitted_results.push(EmittedSearchResult {
            output_range: ByteRange {
                start_byte_offset: start,
                end_byte_offset: text.len(),
            },
            match_index,
        });
    }
    FormattedSearchResults {
        text,
        emitted_results,
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn search_keeps_semantics_separate_from_formatted_ranges(cx: &mut TestAppContext) {
        let source_id = DocumentId::from_value(7);
        let source = cx.new(|_| BufferModel::from_text("α Node\nNode tail"));
        let controller = cx.update(|cx| {
            SearchResultsController::search("Node", source_id, "fixture.rs", source.clone(), cx)
        });

        assert_eq!(controller.title(), "Search: \"Node\"");
        assert_eq!(controller.source_model(), &source);
        assert_eq!(controller.source_revision(), 0);
        assert_eq!(controller.matches().len(), 2);
        assert_eq!(controller.matches()[0].source, source_id);
        assert_eq!(controller.matches()[0].source_range.start_byte_offset, 3);
        assert_eq!(controller.matches()[0].line_number, 1);
        assert_eq!(controller.matches()[0].preview, "α Node");
        assert_eq!(controller.matches()[1].line_number, 2);

        let (generated_text, contributions) = cx.read(|cx| {
            let generated = controller.model().read(cx);
            (generated.text(), generated.resolved_contributions())
        });
        assert_eq!(
            generated_text,
            "fixture.rs:1: α Node\nfixture.rs:2: Node tail"
        );
        assert_eq!(contributions.len(), 2);
        for (contribution, emitted) in contributions.iter().zip(controller.emitted_results()) {
            assert_eq!(contribution.range, emitted.output_range);
            assert_eq!(
                contribution.command.as_deref(),
                Some(ACTIVATE_SEARCH_RESULT_COMMAND)
            );
        }
    }

    #[gpui::test]
    fn target_resolution_uses_output_ranges_and_rejects_stale_sources(cx: &mut TestAppContext) {
        let source_id = DocumentId::from_value(7);
        let source = cx.new(|_| BufferModel::from_text("Node tail"));
        let controller = cx.update(|cx| {
            SearchResultsController::search("Node", source_id, "fixture.rs", source.clone(), cx)
        });
        let output_range = controller.emitted_results()[0].output_range;

        let target = cx.read(|cx| controller.resolve_target(output_range, cx).unwrap());
        assert_eq!(target.source, source_id);
        assert_eq!(target.source_range.start_byte_offset, 0);
        assert!(matches!(
            cx.read(|cx| controller.resolve_target(
                ByteRange {
                    start_byte_offset: output_range.start_byte_offset + 1,
                    ..output_range
                },
                cx,
            )),
            Err(SearchResultActivationError::UnknownResult)
        ));

        source.update(cx, |model, _| {
            model.replace(0..0, "changed ").unwrap();
        });
        assert!(matches!(
            cx.read(|cx| controller.resolve_target(output_range, cx)),
            Err(SearchResultActivationError::StaleSource)
        ));
    }
}
