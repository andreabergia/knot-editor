use super::*;
use crate::core::buffer::TextBuffer;

const RUST_HIGHLIGHTS: &str = r#"
(identifier) @variable
((identifier) @constant
  (#match? @constant "^[A-Z][A-Z0-9_]+$"))
(function_item name: (identifier) @function)
(call_expression function: (identifier) @function.call)
(type_identifier) @type
(primitive_type) @type.builtin
["fn" "let" "if" "else" "struct" "impl"] @keyword
"return" @keyword.return
(line_comment) @comment
(block_comment) @comment
(string_literal) @string
(escape_sequence) @string.escape
(integer_literal) @number
"#;

/// Long enough that every test can finish a foreground parse.
const UNBOUNDED: Duration = Duration::from_secs(60);

fn language(highlights: &str) -> Arc<SyntaxLanguage> {
    Arc::new(SyntaxLanguage::new("rust", highlights).unwrap())
}

/// A buffer and its syntax state, edited together the way a model commit
/// feeds the edit log to the state.
struct Fixture {
    buffer: TextBuffer,
    state: SyntaxState,
}

impl Fixture {
    fn new(text: &str) -> Self {
        Self::with_query(text, RUST_HIGHLIGHTS)
    }

    fn with_query(text: &str, highlights: &str) -> Self {
        let buffer = TextBuffer::from_text(text);
        let mut state = SyntaxState::new(language(highlights));
        assert!(state.parse_in_foreground(&buffer.snapshot(), UNBOUNDED));
        Self { buffer, state }
    }

    fn replace(&mut self, range: Range<usize>, text: &str) {
        let seq = self.buffer.edit_seq();
        self.buffer.replace(range, text);
        self.state.edit(self.buffer.edits_since(seq));
    }

    fn reparse(&mut self) {
        assert!(
            self.state
                .parse_in_foreground(&self.buffer.snapshot(), UNBOUNDED)
        );
    }

    fn text(&self) -> String {
        self.buffer.read_range(0..self.buffer.len())
    }

    fn highlights(&self) -> Vec<(String, &'static str)> {
        self.highlights_in(0..self.buffer.len())
    }

    fn highlights_in(&self, range: Range<usize>) -> Vec<(String, &'static str)> {
        self.state
            .highlights(&self.buffer.snapshot(), range)
            .into_iter()
            .map(|(range, highlight)| (self.buffer.read_range(range), highlight.name()))
            .collect()
    }
}

fn fresh_highlights(text: &str) -> Vec<(String, &'static str)> {
    Fixture::new(text).highlights()
}

fn named(spans: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    spans
        .iter()
        .map(|(text, name)| (text.to_string(), *name))
        .collect()
}

#[test]
fn language_errors_name_the_grammar_or_query_location() {
    assert_eq!(
        SyntaxLanguage::new("cobol", "").err(),
        Some(SyntaxLanguageError::UnknownGrammar)
    );
    let Err(SyntaxLanguageError::InvalidQuery { row, column, .. }) =
        SyntaxLanguage::new("rust", "(identifier) @variable\n(not_a_node) @x")
    else {
        panic!("expected an invalid query");
    };
    assert_eq!((row, column), (1, 1));
}

#[test]
fn captures_paint_in_cursor_order_by_priority() {
    let fixture = Fixture::with_query(
        "fn main() {}",
        r#"
        ((identifier) @constant (#set! priority 90))
        (function_item) @keyword
        (identifier) @variable
        (function_item name: (identifier) @function)
        "#,
    );
    // The later `@function` pattern beats `@variable` on the same node, the
    // inner identifier beats its enclosing item, and the low-priority
    // `@constant` loses to everything despite coming first.
    assert_eq!(
        fixture.highlights(),
        named(&[
            ("fn ", "keyword"),
            ("main", "function"),
            ("() {}", "keyword")
        ])
    );
}

#[test]
fn predicates_and_unknown_captures_filter_highlights() {
    let fixture = Fixture::with_query(
        "let MAX = lower;",
        r#"
        (identifier) @spell
        ((identifier) @constant (#match? @constant "^[A-Z]+$"))
        "#,
    );
    assert_eq!(fixture.highlights(), named(&[("MAX", "constant")]));
}

#[test]
fn highlights_are_limited_and_clipped_to_the_requested_range() {
    let fixture = Fixture::new("let abc = 1;\nlet d = 2;\n");
    assert_eq!(fixture.highlights_in(5..7), named(&[("bc", "variable")]));
    assert_eq!(
        fixture.highlights_in(13..23),
        named(&[("let", "keyword"), ("d", "variable"), ("2", "number")])
    );
}

#[test]
fn an_edited_tree_paints_shifted_highlights_before_reparsing() {
    let mut fixture = Fixture::new("fn a() {}");
    let generation = fixture.state.generation();
    fixture.replace(0..0, "\n\n");
    assert!(fixture.state.generation() > generation);
    assert_eq!(
        fixture
            .state
            .highlights(&fixture.buffer.snapshot(), 0..fixture.buffer.len())[0]
            .0,
        2..4
    );
    fixture.reparse();
    assert_eq!(fixture.highlights(), fresh_highlights(&fixture.text()));
}

#[test]
fn incremental_highlights_match_a_fresh_parse_after_random_edits() {
    let mut fixture = Fixture::new(
        "struct Point { x: i32 }\n\nfn main() {\n    let p = make(1, \"a\\n\");\n    \
         // done\n    if p { return 2; }\n}\n",
    );
    let fragments = [
        "", "x", " ", "\n", "fn ", "let ", "\"", "\\n", "/*", "*/", "// ", "{", "}", "(", ")", ";",
        "return ", "MAX", "42", "é", "call(1)",
    ];
    let mut rng: u64 = 0x5eed_cafe;
    let mut next = |bound: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % bound as u64) as usize
    };
    for step in 0..400 {
        let text = fixture.text();
        let boundaries: Vec<usize> = (0..=text.len())
            .filter(|offset| text.is_char_boundary(*offset))
            .collect();
        let a = boundaries[next(boundaries.len())];
        let b = boundaries[next(boundaries.len())];
        let range = a.min(b)..a.max(b).min(a.min(b) + 12);
        let range = range.start..range.end.max(range.start);
        let range = if text.is_char_boundary(range.end) {
            range
        } else {
            range.start..range.start
        };
        fixture.replace(range, fragments[next(fragments.len())]);
        // Parse after a run of commits as well as after single ones.
        if next(3) > 0 {
            fixture.reparse();
            assert_eq!(
                fixture.highlights(),
                fresh_highlights(&fixture.text()),
                "step {step}: {:?}",
                fixture.text()
            );
        }
    }
}

#[test]
fn a_background_result_is_rebased_over_edits_committed_while_it_ran() {
    let mut fixture = Fixture::new("fn a() {}\n");
    fixture.replace(0..0, "fn b() { let x = 1; }\n");
    let job = fixture
        .state
        .start_background_parse(fixture.buffer.snapshot());
    // The running parse owns the reparse.
    assert!(
        !fixture
            .state
            .parse_in_foreground(&fixture.buffer.snapshot(), UNBOUNDED)
    );
    assert!(!fixture.state.needs_background_parse());

    fixture.replace(0..0, "// note\n");
    let end = fixture.buffer.len();
    fixture.replace(end..end, "fn c() {}\n");
    let parsed = std::thread::spawn(move || job.run()).join().unwrap();
    assert!(fixture.state.finish_background_parse(parsed));

    // Rebased onto the current text but not yet a parse of it.
    assert!(fixture.state.needs_background_parse());
    let shifted = fixture.highlights();
    assert!(shifted.contains(&("b".into(), "function")));
    assert!(shifted.contains(&("a".into(), "function")));
    fixture.reparse();
    assert_eq!(fixture.highlights(), fresh_highlights(&fixture.text()));
}

#[test]
fn a_background_result_without_later_edits_is_current() {
    let buffer = TextBuffer::from_text("fn a() {}\n");
    let mut state = SyntaxState::new(language(RUST_HIGHLIGHTS));
    let job = state.start_background_parse(buffer.snapshot());
    assert!(
        state
            .highlights(&buffer.snapshot(), 0..buffer.len())
            .is_empty()
    );
    assert!(state.finish_background_parse(job.run()));
    assert!(!state.needs_background_parse());
    assert!(state.parse_in_foreground(&buffer.snapshot(), Duration::ZERO));
    assert!(
        !state
            .highlights(&buffer.snapshot(), 0..buffer.len())
            .is_empty()
    );
}

#[test]
fn a_result_for_another_state_is_rejected() {
    let buffer = TextBuffer::from_text("fn a() {}\n");
    let mut replaced = SyntaxState::new(language(RUST_HIGHLIGHTS));
    let job = replaced.start_background_parse(buffer.snapshot());

    let mut state = SyntaxState::new(language(RUST_HIGHLIGHTS));
    let pending = state.start_background_parse(buffer.snapshot());
    let generation = state.generation();
    assert!(!state.finish_background_parse(job.run()));
    assert_eq!(state.generation(), generation);
    assert!(
        state
            .highlights(&buffer.snapshot(), 0..buffer.len())
            .is_empty()
    );

    let parsed = pending.run();
    let duplicate = ParsedTree {
        id: parsed.id,
        tree: parsed.tree.clone(),
    };
    assert!(state.finish_background_parse(parsed));
    assert!(!state.finish_background_parse(duplicate));
}

#[test]
fn a_parse_over_budget_is_cancelled_and_can_restart() {
    let source = "fn f() { let x = call(1, \"s\"); }\n".repeat(20_000);
    let buffer = TextBuffer::from_text(source);
    let mut state = SyntaxState::new(language(RUST_HIGHLIGHTS));
    assert!(!state.parse_in_foreground(&buffer.snapshot(), Duration::ZERO));
    assert!(state.needs_background_parse());
    assert!(state.parse_in_foreground(&buffer.snapshot(), UNBOUNDED));
    assert_eq!(
        state.highlights(&buffer.snapshot(), 0..2),
        vec![(0..2, Highlight::named("keyword"))]
    );
}

/// Parse and query costs that set `FOREGROUND_BUDGET`. Run with
/// `cargo test --release --lib syntax_costs -- --ignored --nocapture`.
#[test]
#[ignore]
fn syntax_costs_on_large_rust_files() {
    let unit = "/// Documented.\nfn item(value: &str) -> Result<usize, String> {\n    \
                let parsed = value.parse::<usize>().map_err(|e| e.to_string())?;\n    \
                if parsed > 10 { return Ok(parsed * 2); }\n    Ok(parsed)\n}\n\n";
    let percentile = |samples: &mut Vec<Duration>, p: usize| {
        samples.sort();
        samples[samples.len() * p / 100]
    };
    for units in [500, 10_000] {
        let source = unit.repeat(units);
        let lines = source.lines().count();
        let mut fixture = Fixture {
            buffer: TextBuffer::from_text(source),
            state: SyntaxState::new(language(RUST_HIGHLIGHTS)),
        };
        let started = Instant::now();
        fixture.reparse();
        println!(
            "{lines} lines, {} KiB: initial parse {:?}",
            fixture.buffer.len() / 1024,
            started.elapsed()
        );

        // Extend an identifier, which keeps the text valid, then type
        // characters that leave it invalid until the statement completes.
        let middle = fixture.buffer.len() / 2;
        let mut offset = fixture.text()[middle..].find("parsed =").unwrap() + middle + 6;
        for (label, typed) in [("valid", "x"), ("invalid", "(")] {
            let mut typing = Vec::new();
            for _ in 0..300 {
                fixture.replace(offset..offset, typed);
                offset += 1;
                let started = Instant::now();
                fixture.reparse();
                typing.push(started.elapsed());
            }
            println!(
                "  {label} typing reparse p50 {:?} p99 {:?}",
                percentile(&mut typing, 50),
                percentile(&mut typing, 99)
            );
        }

        let mut queries = Vec::new();
        for _ in 0..200 {
            let start = fixture.buffer.line_start(lines / 2);
            let end = fixture.buffer.line_start(lines / 2 + 60);
            let started = Instant::now();
            std::hint::black_box(
                fixture
                    .state
                    .highlights(&fixture.buffer.snapshot(), start..end),
            );
            queries.push(started.elapsed());
        }
        println!(
            "  60-line highlight query p50 {:?} p99 {:?}",
            percentile(&mut queries, 50),
            percentile(&mut queries, 99)
        );
    }
}
