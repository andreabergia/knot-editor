use gpui::*;
use libghostty_vt::{
    RenderState, Terminal, TerminalOptions,
    render::{CellIterator, RowIterator},
};

const COLUMNS: u16 = 40;
const ROWS: u16 = 8;
const REPLAY_CHUNK_SIZE: usize = 13;

struct Recording {
    name: &'static str,
    bytes: &'static [u8],
}

const RECORDINGS: &[Recording] = &[
    Recording {
        name: "styles-and-unicode",
        bytes: concat!(
            "\x1b[1;34mbold blue\x1b[0m ",
            "\x1b[3;38;2;255;128;0mitalic orange\x1b[0m\r\n",
            "ASCII | e\u{301} | 👩🏽‍💻 | 漢字 | \r\n",
            "\x1b[4;9munderline strike\x1b[0m\r\n",
        )
        .as_bytes(),
    },
    Recording {
        name: "cursor-and-scrollback",
        bytes: concat!(
            "line 00\r\nline 01\r\nline 02\r\nline 03\r\n",
            "line 04\r\nline 05\r\nline 06\r\nline 07\r\n",
            "line 08\r\nline 09\r\nline 10\r\nline 11\r\n",
            "\x1b[2A\x1b[6C\x1b[31mEDIT\x1b[0m",
            "\x1b[2;1H\x1b[2Krewritten row",
        )
        .as_bytes(),
    },
    Recording {
        name: "alternate-screen",
        bytes: concat!(
            "\x1b[?1049h\x1b[2J\x1b[H",
            "\x1b[7m alternate screen \x1b[0m",
            "\x1b[8;1Hstatus: ready",
            "\x1b[?25l\x1b[?25h\x1b[?1049l",
        )
        .as_bytes(),
    },
];

fn main() -> anyhow::Result<()> {
    let inspections = replay_recordings()?;
    if std::env::args().any(|argument| argument == "--gui") {
        show_grid(inspections);
    }
    Ok(())
}

fn replay_recordings() -> anyhow::Result<Vec<(&'static str, RenderInspection)>> {
    let mut bytes_replayed = 0;
    let mut inspections = Vec::with_capacity(RECORDINGS.len());

    for recording in RECORDINGS {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: COLUMNS,
            rows: ROWS,
            max_scrollback: 100,
        })?;

        for chunk in recording.bytes.chunks(REPLAY_CHUNK_SIZE) {
            terminal.vt_write(chunk);
        }
        bytes_replayed += recording.bytes.len();

        let inspection = inspect_render_state(&terminal)?;
        println!(
            "replayed {} ({} bytes): {}x{}, {} text cells, {} styled cells, {} colored cells, cursor={:?}",
            recording.name,
            recording.bytes.len(),
            inspection.cols,
            inspection.rows,
            inspection.text_cells,
            inspection.styled_cells,
            inspection.colored_cells,
            inspection.cursor,
        );
        for row in &inspection.visible_rows {
            println!("  {row:?}");
        }
        inspections.push((recording.name, inspection));
    }

    println!(
        "fed {bytes_replayed} recorded bytes into a {COLUMNS}x{ROWS} Ghostty terminal without a PTY"
    );
    Ok(inspections)
}

#[derive(Debug)]
struct RenderInspection {
    cols: u16,
    rows: u16,
    text_cells: usize,
    styled_cells: usize,
    colored_cells: usize,
    cursor: Option<(u16, u16)>,
    visible_rows: Vec<String>,
}

fn inspect_render_state(terminal: &Terminal<'_, '_>) -> anyhow::Result<RenderInspection> {
    let mut render_state = RenderState::new()?;
    let mut rows = RowIterator::new()?;
    let mut cells = CellIterator::new()?;
    let snapshot = render_state.update(terminal)?;

    let mut text_cells = 0;
    let mut styled_cells = 0;
    let mut colored_cells = 0;
    let mut visible_rows = Vec::with_capacity(snapshot.rows()?.into());
    let mut row_iter = rows.update(&snapshot)?;

    while let Some(row) = row_iter.next() {
        let mut visible_row = String::new();
        let mut cell_iter = cells.update(row)?;

        while let Some(cell) = cell_iter.next() {
            let graphemes = cell.graphemes()?;
            if graphemes.is_empty() {
                visible_row.push(' ');
            } else {
                text_cells += 1;
                visible_row.extend(graphemes);
            }
            styled_cells += usize::from(cell.has_styling()?);
            colored_cells += usize::from(cell.fg_color()?.is_some() || cell.bg_color()?.is_some());
        }

        visible_rows.push(visible_row.trim_end().to_owned());
    }

    let cursor = snapshot
        .cursor_visible()?
        .then(|| snapshot.cursor_viewport())
        .transpose()?
        .flatten()
        .map(|cursor| (cursor.x, cursor.y));

    Ok(RenderInspection {
        cols: snapshot.cols()?,
        rows: snapshot.rows()?,
        text_cells,
        styled_cells,
        colored_cells,
        cursor,
        visible_rows,
    })
}

struct GhosttyGrid {
    recordings: Vec<(&'static str, RenderInspection)>,
}

impl Render for GhosttyGrid {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(rgb(0x181818))
            .text_color(rgb(0xd4d4d4));

        for (name, inspection) in &self.recordings {
            let mut grid = div()
                .flex()
                .flex_col()
                .font_family("Menlo")
                .text_size(px(13.))
                .line_height(px(18.));
            for row in &inspection.visible_rows {
                grid = grid.child(div().h(px(18.)).child(row.clone()));
            }

            root = root.child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x8f8f8f))
                            .child(format!("{name} · {}×{}", inspection.cols, inspection.rows)),
                    )
                    .child(grid),
            );
        }

        root
    }
}

fn show_grid(recordings: Vec<(&'static str, RenderInspection)>) {
    Application::new().run(move |app: &mut App| {
        app.on_window_closed(|app| {
            if app.windows().is_empty() {
                app.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(560.), px(620.)), app);
        app.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_, cx| cx.new(|_| GhosttyGrid { recordings }),
        )
        .expect("open Ghostty spike window");
        app.activate(true);
    });
}
